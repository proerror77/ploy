# Sports Betting Skill - Claude Agent SDK 集成指南

这是一个完整的 Claude Agent SDK skill 实现，可以让 Claude 调用你的 Polymarket 运动策略分析功能。

## 📦 文件结构

```
.claude/skills/
├── sports-bet.md          # Skill 文档（Claude 读取）
├── sports-bet.py          # Python 实现
├── sports-bet.ts          # TypeScript 实现
└── README.md              # 本文件
```

## 🎯 Skill 功能

这个 skill 让 Claude 能够：

1. **分析 Polymarket 运动事件**
   - 解析事件 URL
   - 提取球队和联赛信息

2. **收集多源数据**（通过 Grok）
   - 球员状态和伤病
   - 博彩赔率
   - 专家预测
   - 突发新闻
   - 历史数据

3. **AI 分析**（通过 Claude Opus）
   - 预测胜率
   - 计算边缘
   - 生成交易建议

4. **可选功能**
   - DraftKings 赔率对比
   - 套利机会检测
   - 自定义边缘阈值

## 🚀 使用方法

### 在 Claude 对话中调用

用户可以这样与 Claude 对话：

```
用户: 帮我分析这场 NBA 比赛：
https://polymarket.com/event/nba-phi-dal-2026-01-11

Claude: 我来为你分析这场比赛。
[调用 sports_bet_analysis 工具]

分析结果：
- 76ers 胜率预测：58.5%
- 市场赔率：45.0%
- 边缘：+13.5%
- 建议：买入 76ers YES
- 仓位：8.2% 资金

关键因素：
• Embiid 伤愈复出，最近 5 场场均 32.5 分
• 76ers 主场战绩 15-5
• Mavericks 客场三连战疲劳
```

### Python Agent SDK 集成

```python
from anthropic import Anthropic
from skills.sports_bet import TOOL_DEFINITION, handle_tool_call

client = Anthropic(api_key="your-api-key")

# 定义工具
tools = [TOOL_DEFINITION]

# 创建对话
response = client.messages.create(
    model="claude-3-5-sonnet-20241022",
    max_tokens=4096,
    tools=tools,
    messages=[{
        "role": "user",
        "content": "分析这场比赛：https://polymarket.com/event/nba-phi-dal-2026-01-11"
    }]
)

# 处理工具调用
for block in response.content:
    if block.type == "tool_use":
        result = await handle_tool_call(block.input)
        print(result)
```

### TypeScript Agent SDK 集成

```typescript
import Anthropic from '@anthropic-ai/sdk';
import { TOOL_DEFINITION, handleToolCall, formatResult } from './skills/sports-bet';

const client = new Anthropic({
  apiKey: process.env.ANTHROPIC_API_KEY
});

// 创建对话
const response = await client.messages.create({
  model: 'claude-3-5-sonnet-20241022',
  max_tokens: 4096,
  tools: [TOOL_DEFINITION],
  messages: [{
    role: 'user',
    content: '分析这场比赛：https://polymarket.com/event/nba-phi-dal-2026-01-11'
  }]
});

// 处理工具调用
for (const block of response.content) {
  if (block.type === 'tool_use') {
    const result = await handleToolCall(block.input);
    console.log(formatResult(result));
  }
}
```

## 🔧 配置要求

### 环境变量

```bash
# 必需
export GROK_API_KEY="your-grok-api-key"
export ANTHROPIC_API_KEY="your-claude-api-key"

# 可选（用于 DraftKings 对比）
export THE_ODDS_API_KEY="your-odds-api-key"
```

### 依赖项

**Python:**
```bash
pip install anthropic
```

**TypeScript:**
```bash
npm install @anthropic-ai/sdk
```

**Rust CLI:**
```bash
cargo build --release
# 确保 ploy 在 PATH 中
```

## 📋 Tool Definition

```json
{
  "name": "sports_bet_analysis",
  "description": "Analyze sports betting opportunities on Polymarket using AI-powered multi-source analysis",
  "input_schema": {
    "type": "object",
    "properties": {
      "url": {
        "type": "string",
        "description": "Polymarket event URL"
      },
      "compareDraftkings": {
        "type": "boolean",
        "description": "Include DraftKings odds comparison",
        "default": false
      },
      "minEdge": {
        "type": "number",
        "description": "Minimum edge percentage to recommend",
        "default": 5.0
      }
    },
    "required": ["url"]
  }
}
```

## 🎨 输出格式

### 成功响应

```json
{
  "success": true,
  "game": {
    "league": "NBA",
    "team1": "Philadelphia 76ers",
    "team2": "Dallas Mavericks"
  },
  "market_odds": {
    "team1_yes": 0.450,
    "team1_no": 0.550,
    "team2_yes": 0.550,
    "team2_no": 0.450
  },
  "prediction": {
    "team1_win_prob": 0.585,
    "team2_win_prob": 0.415,
    "confidence": 0.78,
    "reasoning": "Embiid upgraded to probable...",
    "key_factors": [
      "Embiid return from injury",
      "Home court advantage",
      "Mavericks fatigue factor"
    ]
  },
  "recommendation": {
    "action": "Buy",
    "side": "76ers YES",
    "edge": 13.5,
    "suggested_size": 8.2,
    "reasoning": "Predicted 58.5% vs market 45.0%"
  }
}
```

### 错误响应

```json
{
  "success": false,
  "error": "Missing required environment variables: GROK_API_KEY",
  "help": "Set GROK_API_KEY and ANTHROPIC_API_KEY in your environment"
}
```

## 🔍 工作流程

```
用户请求
    ↓
Claude 识别需要分析运动事件
    ↓
调用 sports_bet_analysis 工具
    ↓
Python/TS 包装器调用 Rust CLI
    ↓
Rust 执行分析：
    1. 解析 URL
    2. Grok 收集数据（7 步）
    3. Claude Opus 分析
    4. 计算边缘和建议
    ↓
返回结构化结果
    ↓
Claude 格式化并呈现给用户
```

## 📊 性能指标

- **平均响应时间**: 30-60 秒
- **数据收集**: 7 个并行 API 调用
- **Claude Opus 超时**: 5 分钟
- **成功率**: ~95%（取决于数据源可用性）

## 🛡️ 错误处理

Skill 会优雅处理：

1. **缺少环境变量**: 返回友好提示
2. **无效 URL**: 解析错误提示
3. **API 失败**: 降级到部分分析
4. **超时**: 2 分钟后返回超时错误
5. **市场不存在**: 提示市场未找到

## 🔄 扩展建议

### 1. 批量分析
```python
async def batch_analyze(urls: list[str]) -> list[dict]:
    tasks = [handle_tool_call({"url": url}) for url in urls]
    return await asyncio.gather(*tasks)
```

### 2. 实时监控
```python
async def watch_game(url: str, interval: int = 60):
    while True:
        result = await handle_tool_call({"url": url})
        if result["recommendation"]["edge"] > 10:
            send_notification(result)
        await asyncio.sleep(interval)
```

### 3. 历史追踪
```python
def save_analysis(result: dict):
    db.insert({
        "timestamp": datetime.now(),
        "game": result["game"],
        "prediction": result["prediction"],
        "recommendation": result["recommendation"]
    })
```

## 📚 相关资源

- [Claude Agent SDK 文档](https://docs.anthropic.com/claude/docs/agent-sdk)
- [Tool Use 指南](https://docs.anthropic.com/claude/docs/tool-use)
- [Polymarket API](https://docs.polymarket.com/)
- [Grok API](https://docs.x.ai/api)

## 🐛 调试

### 启用详细日志

```bash
# Python
export LOG_LEVEL=DEBUG
python -m skills.sports_bet

# TypeScript
DEBUG=* node skills/sports-bet.ts
```

### 测试 CLI 直接调用

```bash
ploy sports bet \
  --url "https://polymarket.com/event/nba-phi-dal-2026-01-11" \
  --format json
```

### 验证环境变量

```bash
echo $GROK_API_KEY
echo $ANTHROPIC_API_KEY
echo $THE_ODDS_API_KEY
```

## 💡 最佳实践

1. **缓存结果**: 相同 URL 在 5 分钟内使用缓存
2. **速率限制**: 每分钟最多 10 次分析
3. **错误重试**: API 失败时最多重试 3 次
4. **超时设置**: 根据网络情况调整超时时间
5. **日志记录**: 记录所有分析请求和结果

## 📞 支持

如有问题，请：
1. 检查环境变量配置
2. 验证 Rust CLI 可用性
3. 查看日志输出
4. 提交 Issue 到项目仓库
