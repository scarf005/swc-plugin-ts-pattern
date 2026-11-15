import { match, P } from "ts-pattern"

type Text = { type: "text"; data: string }
type Img = { type: "img"; data: { src: string; alt: string } }
type Video = { type: "video"; data: { src: string; format: "mp4" | "webm" } }
type Content = Text | Img | Video

export const inputs = [
  { type: "text", data: "hello" },
  { type: "video", data: { src: "/a.mp4", format: "webm" } },
  { type: "img", data: { src: "/a.png", alt: "A" } },
] satisfies Content[]

export const run = (content: Content): string =>
  match(content)
    .with({ type: "text", data: P.select() }, (value) => `<p>${value}</p>`)
    .with({ type: P.union("img", "video"), data: P.select() }, (data) => JSON.stringify(data))
    .exhaustive()
