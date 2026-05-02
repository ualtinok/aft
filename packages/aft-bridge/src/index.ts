/**
 * @cortexkit/aft-bridge
 *
 * Shared transport, binary resolution, and ONNX runtime helpers for AFT
 * agent-host plugins. Public surface intentionally narrow — host policies
 * (config loading, permission UX, tool registration, notifications) stay in
 * each host plugin.
 */

// --- logger contract ---
export { setActiveLogger } from "./active-logger.js";
export type {
  BashCompletedPayload,
  BridgeOptions,
  BridgeRequestOptions,
  ConfigureWarning,
  ConfigureWarningsContext,
} from "./bridge.js";
// --- transport ---
export { BinaryBridge, compareSemver } from "./bridge.js";
// --- binary resolution ---
export {
  downloadBinary,
  ensureBinary,
  getBinaryName,
  getCacheDir,
  getCachedBinaryPath,
} from "./downloader.js";
export type { Logger, LogMeta } from "./logger.js";
// --- ONNX runtime ---
export {
  cleanupOnnxRuntime,
  ensureOnnxRuntime,
  getManualInstallHint,
  isOrtAutoDownloadSupported,
} from "./onnx-runtime.js";
// --- platform helpers ---
export { PLATFORM_ARCH_MAP, PLATFORM_ASSET_MAP } from "./platform.js";
export type { PoolOptions } from "./pool.js";
export { BridgePool } from "./pool.js";
// --- wire contract ---
export type {
  AftErrorResponse,
  AftPushFrame,
  AftRequestEnvelope,
  AftResponse,
  AftSuccessResponse,
  BashCompletedFrame,
  BgCompletion,
  ConfigureWarningFrame,
  PermissionAskFrame,
  ProgressFrame,
} from "./protocol.js";
export { findBinary, findBinarySync, platformKey } from "./resolver.js";
