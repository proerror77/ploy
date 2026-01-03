#!/bin/bash
# Simple AWS deployment for Ploy CRYINGLITTLEBABY strategy
# Builds Docker image locally, pushes to ECR, runs on EC2
set -e

AWS_REGION="ap-northeast-1"
ECR_REPO="ploy-trading"
INSTANCE_TYPE="t3.micro"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

echo -e "${GREEN}=== Ploy AWS Japan Deployment ===${NC}"

# Step 1: Check AWS credentials
echo -e "${YELLOW}[1/5] Checking AWS credentials...${NC}"
AWS_ACCOUNT=$(aws sts get-caller-identity --query Account --output text --region $AWS_REGION) || {
    echo -e "${RED}AWS not configured. Run: aws configure${NC}"
    exit 1
}
echo "Account: $AWS_ACCOUNT"

# Step 2: Create ECR repository if not exists
echo -e "${YELLOW}[2/5] Setting up ECR repository...${NC}"
aws ecr describe-repositories --repository-names $ECR_REPO --region $AWS_REGION 2>/dev/null || \
aws ecr create-repository --repository-name $ECR_REPO --region $AWS_REGION

ECR_URI="$AWS_ACCOUNT.dkr.ecr.$AWS_REGION.amazonaws.com/$ECR_REPO"

# Step 3: Build and push Docker image
echo -e "${YELLOW}[3/5] Building Docker image...${NC}"
docker build -t $ECR_REPO:latest .

echo -e "${YELLOW}[4/5] Pushing to ECR...${NC}"
aws ecr get-login-password --region $AWS_REGION | docker login --username AWS --password-stdin $ECR_URI
docker tag $ECR_REPO:latest $ECR_URI:latest
docker push $ECR_URI:latest

# Step 5: Create run command
echo -e "${YELLOW}[5/5] Generating run command...${NC}"

cat <<EOF

${GREEN}=== Deployment Ready ===${NC}

To run on AWS EC2 (ap-northeast-1):

1. Launch EC2 instance (Amazon Linux 2023, t3.micro)
2. SSH into instance
3. Run these commands:

# Install Docker
sudo yum install -y docker
sudo systemctl start docker
sudo usermod -aG docker ec2-user

# Login to ECR
aws ecr get-login-password --region $AWS_REGION | docker login --username AWS --password-stdin $ECR_URI

# Run Ploy
docker run -d \\
  --name ploy-trading \\
  -e POLYMARKET_PRIVATE_KEY="\$POLYMARKET_PRIVATE_KEY" \\
  -e RUST_LOG=info,ploy=debug \\
  --restart unless-stopped \\
  $ECR_URI:latest \\
  momentum \\
  --symbols BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT \\
  --min-move 0.2 \\
  --max-entry 35 \\
  --shares 100

# View logs
docker logs -f ploy-trading

EOF
