import { match } from "ts-pattern"

type Text = { type: "text"; data: string }
type Img = { type: "img"; data: { src: string; alt: string } }
type Video = { type: "video"; data: { src: string; format: "mp4" | "webm" } }
type Content = Text | Img | Video

export const inputs = [
  { type: "text", data: "hello" },
  { type: "img", data: { src: "/a.png", alt: "A" } },
  { type: "video", data: { src: "/a.mp4", format: "mp4" } },
] satisfies Content[]

export const run = (content: Content): string =>
  match(content)
    .with({ type: "text" }, () => "<p>...</p>")
    .with({ type: "img" }, () => "<img ... />")
    .with({ type: "video" }, () => "<video ... />")
    .exhaustive()
