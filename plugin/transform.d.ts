export type TransformTsPatternOptions = {
  filename?: string
  pluginPath?: string
  pluginOptions?: { onFallback?: "warn" | "error" }
  sourceMaps?: boolean | "inline"
  swcOptions?: Record<string, unknown>
}

export type TransformTsPatternResult = {
  code: string
  map?: string
  diagnostics?: unknown[]
}

export const defaultPluginPath: () => Promise<string>
export const transformTsPattern: (
  source: string,
  options?: TransformTsPatternOptions,
) => Promise<TransformTsPatternResult>
export const transformTsPatternSync: (
  source: string,
  options?: TransformTsPatternOptions,
) => TransformTsPatternResult
