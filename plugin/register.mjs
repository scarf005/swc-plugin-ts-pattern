// @ts-self-types="./register.d.ts"
import { registerHooks } from "node:module"
import { initialize, load, resolve } from "./loader.mjs"
import { defaultPluginPath } from "./transform.mjs"

const env = () => {
  try {
    return globalThis.process?.env?.SWC_PLUGIN_TS_PATTERN_PATH
  } catch {
    return undefined
  }
}

const pluginPath = env() ?? await defaultPluginPath()

initialize({ pluginPath })
registerHooks({ load, resolve })
