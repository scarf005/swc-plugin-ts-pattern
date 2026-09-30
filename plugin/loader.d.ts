import type { LoadHookSync, ResolveHookSync } from "node:module"
import type { TransformTsPatternOptions } from "./transform.mjs"

export type LoaderOptions = TransformTsPatternOptions

export const initialize: (data?: LoaderOptions) => void
export const resolve: ResolveHookSync
export const load: LoadHookSync
