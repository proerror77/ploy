#!/bin/bash
# Deploy Ploy CRYINGLITTLEBABY strategy to AWS Japan (ap-northeast-1)
# Usage: ./scripts/deploy-aws-jp.sh

set -e

# Configuration
AWS_REGION="ap-northeast-1"
INSTANCE_TYPE="t3.micro"  # Minimal - can upgrade if needed
KEY_NAME="${AWS_KEY_NAME:-ploy-jp}"
SECURITY_GROUP="ploy-trading-sg"
AMI_ID="ami-0d52744d6551d851e"  # Amazon Linux 2023 in ap-northeast-1

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}=== Deploying Ploy to AWS Japan ===${NC}"
echo "Region: $AWS_REGION"
echo "Instance Type: $INSTANCE_TYPE"

# Check AWS CLI
if ! command -v aws &> /dev/null; then
    echo "AWS CLI not installed. Install with: brew install awscli"
    exit 1
fi

# Check authentication
echo -e "${YELLOW}Checking AWS credentials...${NC}"
aws sts get-caller-identity --region $AWS_REGION || {
    echo "AWS not authenticated. Run: aws configure"
    exit 1
}

# Create security group if not exists
echo -e "${YELLOW}Setting up security group...${NC}"
aws ec2 describe-security-groups --group-names $SECURITY_GROUP --region $AWS_REGION 2>/dev/null || \
aws ec2 create-security-group \
    --group-name $SECURITY_GROUP \
    --description "Ploy Trading Bot Security Group" \
    --region $AWS_REGION

# Allow SSH (optional, for debugging)
aws ec2 authorize-security-group-ingress \
    --group-name $SECURITY_GROUP \
    --protocol tcp \
    --port 22 \
    --cidr 0.0.0.0/0 \
    --region $AWS_REGION 2>/dev/null || true

# Create key pair if not exists
echo -e "${YELLOW}Setting up key pair...${NC}"
if ! aws ec2 describe-key-pairs --key-names $KEY_NAME --region $AWS_REGION 2>/dev/null; then
    aws ec2 create-key-pair \
        --key-name $KEY_NAME \
        --query 'KeyMaterial' \
        --output text \
        --region $AWS_REGION > ~/.ssh/${KEY_NAME}.pem
    chmod 400 ~/.ssh/${KEY_NAME}.pem
    echo "Key saved to ~/.ssh/${KEY_NAME}.pem"
fi

# User data script to install and run ploy
USER_DATA=$(cat <<'EOF'
#!/bin/bash
set -e

# Install dependencies
yum update -y
yum install -y docker git

# Start Docker
systemctl start docker
systemctl enable docker

# Create ploy directory
mkdir -p /opt/ploy
cd /opt/ploy

# Pull and run the container (will build from source)
# For now, we'll install Rust and build directly
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Clone the repo (or copy from S3)
git clone https://github.com/your-repo/ploy.git /opt/ploy/src
cd /opt/ploy/src

# Build
features="${PLOY_CARGO_FEATURES:-rl,onnx,api}"
cargo build --release --features "$features"

# Create env file
cat > /opt/ploy/.env <<'ENVEOF'
POLYMARKET_PRIVATE_KEY=${POLYMARKET_PRIVATE_KEY}
RUST_LOG=info,ploy=debug
ENVEOF

# Run the momentum strategy
nohup /opt/ploy/src/target/release/ploy momentum \
    --symbols BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT \
    --min-move 0.2 \
    --max-entry 35 \
    --shares 100 \
    > /opt/ploy/ploy.log 2>&1 &

echo "Ploy started!"
EOF
)

# Launch EC2 instance
echo -e "${YELLOW}Launching EC2 instance...${NC}"
INSTANCE_ID=$(aws ec2 run-instances \
    --image-id $AMI_ID \
    --instance-type $INSTANCE_TYPE \
    --key-name $KEY_NAME \
    --security-groups $SECURITY_GROUP \
    --user-data "$USER_DATA" \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=ploy-trading-jp}]" \
    --region $AWS_REGION \
    --query 'Instances[0].InstanceId' \
    --output text)

echo -e "${GREEN}Instance launched: $INSTANCE_ID${NC}"

# Wait for instance to be running
echo -e "${YELLOW}Waiting for instance to start...${NC}"
aws ec2 wait instance-running --instance-ids $INSTANCE_ID --region $AWS_REGION

# Get public IP
PUBLIC_IP=$(aws ec2 describe-instances \
    --instance-ids $INSTANCE_ID \
    --region $AWS_REGION \
    --query 'Reservations[0].Instances[0].PublicIpAddress' \
    --output text)

echo ""
echo -e "${GREEN}=== Deployment Complete ===${NC}"
echo "Instance ID: $INSTANCE_ID"
echo "Public IP: $PUBLIC_IP"
echo "Region: $AWS_REGION"
echo ""
echo "SSH Access:"
echo "  ssh -i ~/.ssh/${KEY_NAME}.pem ec2-user@$PUBLIC_IP"
echo ""
echo "View logs:"
echo "  ssh -i ~/.ssh/${KEY_NAME}.pem ec2-user@$PUBLIC_IP 'tail -f /opt/ploy/ploy.log'"
echo ""
echo -e "${YELLOW}NOTE: Set POLYMARKET_PRIVATE_KEY in /opt/ploy/.env on the instance${NC}"
