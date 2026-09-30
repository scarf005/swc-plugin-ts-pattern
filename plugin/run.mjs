#!/usr/bin/env node
// @ts-self-types="./run.d.ts"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

const isDeno = typeof Deno !== "undefined"
const args = isDeno ? Deno.args : process.argv.slice(2)
const [entry, ...entryArgs] = args

if (!entry || entry.startsWith("-")) {
  const status = entry === "-h" || entry === "--help" ? 0 : 1
  console.error("Usage: ts-pattern-swc <entry.ts> [...args]")
  if (isDeno) Deno.exit(status)
  process.exitCode = status
} else {
  await import("./register.mjs")
  if (isDeno) Deno.args.splice(0, 1)
  else process.argv = [process.argv[0], resolve(entry), ...entryArgs]
  await import(pathToFileURL(resolve(entry)).href)
}
