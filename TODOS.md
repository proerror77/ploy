# TODOS

## V2 Migration Follow-ups

### reconcile_fills N+1 优化
- **What:** 当前 reconcile_fills() 对每个 tracked_order 做独立分页 API 调用
- **Why:** N+1 模式，交易量增加后会成为瓶颈
- **Pros:** 用 maker_address 一次查询所有 trades 可将 N 次请求降到 1 次
- **Cons:** 需要在本地按 token_id 匹配，逻辑稍复杂
- **Context:** SDK TradesRequest 支持 maker_address 过滤。当前交易量 ~341/天，不紧急
- **Depends on:** V2 迁移完成

### V2 fee model 验证
- **What:** 验证 V2 费用模型是否影响 PnL 和 fill 计算
- **Why:** V2 费用可能是动态的、仅 taker 侧收取，与 V1 不同
- **Pros:** 确保 PnL 报告准确
- **Cons:** 可能需要重写 fee 计算逻辑
- **Context:** Codex outside voice 指出 V2 fees are dynamic, taker-only。当前代码保留了 V1 的 fee_rate_bps 计算路径
- **Depends on:** V2 上线后可用实际数据验证
