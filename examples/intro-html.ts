import { match, P } from "ts-pattern"

export type Data = { type: "text"; content: string } | { type: "img"; src: string }

export type Result = { type: "ok"; data: Data } | { type: "error"; error: Error }

export const inputs = [
  { type: "ok", data: { type: "text", content: "Hello" } },
  { type: "ok", data: { type: "img", src: "/logo.png" } },
  { type: "error", error: new Error("boom") },
] satisfies Result[]

export const run = (result: Result): string =>
  match(result)
    .with({ type: "error" }, () => "<p>Oups! An error occured</p>")
    .with({ type: "ok", data: { type: "text" } }, (res) => `<p>${res.data.content}</p>`)
    .with({ type: "ok", data: { type: "img", src: P.select() } }, (src) => `<img src="${src}" />`)
    .exhaustive()
