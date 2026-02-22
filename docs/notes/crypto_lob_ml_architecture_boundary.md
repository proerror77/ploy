# crypto_lob_ml 架构层 vs 策略层边界

## 1. 架构层（System / Runtime）
- `PlatformBootstrapConfig::from_app_config` 负责把环境变量注入策略配置（如 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION`）。
- 模型加载失败时属于启动阻断（fail-fast），不会降级到旧启发式或 collect-only。
- 风控和下单准入由 Coordinator 统一负责（本次未改变这层职责）。

## 2. 策略层（Signal / Decision）
- `crypto_lob_ml` 用 LOB 特征输出 `p_up_model`。
- `p_up_window` 仅作为 safety fallback 小权重参与，不再主导。
- 当前默认混合：`p_up = 0.90 * p_up_model + 0.10 * p_up_window`（可用 env 调整窗口权重）。
- 退出默认走 `ev_exit`：当市场价格高于模型公平价值时退出（纯模型驱动）。
- 进场默认走 `lagging_only`：只考虑更便宜的一侧做 EV 判断。
- 5m 市场仅允许在前 `120s` 内进场（可用 env 调整）。
- 15m 市场仅允许在最后 `180s` 内进场（可用 env 调整）。
- `ev_exit_buffer` 提供退出阈值，避免过度抖动。
- `ev_exit_vol_scale` 让退出阈值随窗口不确定性动态放大。

## 3. 训练层（Offline ML）
- 新训练入口：`scripts/train_crypto_lob_tcn_onnx_from_db.py --source sync_records --horizon 5m|15m`。
- 标签来自 `pm_token_settlements`（resolved 结果），特征来自 `sync_records`（LOB + 短周期动量）。
- 训练产物通过 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION` 接入 runtime。

## 4. 上线清单
- 参考：`docs/CRYPTO_LOB_ML_DEPLOY_CHECKLIST.md`
