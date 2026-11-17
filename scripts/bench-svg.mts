// Renders the `BENCH_JSON` output of web/ts-pattern.bench.ts as an SVG table.
// Usage: node scripts/bench-svg.mts <bench.json> <out.svg> <caption>
import { readFile, writeFile } from "node:fs/promises"

const [input, output, caption = ""] = process.argv.slice(2)
if (!input || !output) throw new Error("usage: bench-svg.mts <bench.json> <out.svg> [caption]")

const results: Record<string, Record<string, number>> = JSON.parse(await readFile(input, "utf8"))

const ops = (hz?: number) =>
  hz === undefined ? "-" : hz >= 1e6 ? `${(hz / 1e6).toFixed(2)}M` : `${(hz / 1e3).toFixed(1)}k`

const header = ["case", "ts-pattern", "swc plugin", "native", "speedup"]
const rows = Object.entries(results).map(([id, hz]) => [
  id.replace(/^ts-pattern-benchmark\//, ""),
  ops(hz["ts-pattern raw"]),
  ops(hz["ts-pattern swc plugin"]),
  ops(hz["native code"]),
  `${(hz["ts-pattern swc plugin"]! / hz["ts-pattern raw"]!).toFixed(1)}x`,
])

const charWidth = 7.8
const pad = 12
const rowHeight = 22
const widths = header.map(
  (title, column) =>
    Math.max(title.length, ...rows.map((row) => row[column]!.length)) * charWidth + pad * 2,
)
const width = widths.reduce((sum, value) => sum + value, 0)
const top = caption ? rowHeight : 0
const height = top + rowHeight * (rows.length + 1)

const escape = (text: string) =>
  text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")

const cells = (row: string[], y: number, className: string) =>
  row
    .map((text, column) => {
      const left = widths.slice(0, column).reduce((sum, value) => sum + value, 0)
      // Numbers align right; the case name aligns left.
      const [x, anchor] =
        column === 0 ? [left + pad, "start"] : [left + widths[column]! - pad, "end"]
      return `<text class="${className}" x="${x}" y="${y + 15}" text-anchor="${anchor}">${escape(text)}</text>`
    })
    .join("")

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">
<style>
text { font: 13px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; fill: #1f2328; }
.head { font-weight: bold; }
.caption { fill: #59636e; }
rect { fill: #ffffff; }
.stripe { fill: #f6f8fa; }
@media (prefers-color-scheme: dark) {
  text { fill: #f0f6fc; }
  .caption { fill: #9198a1; }
  rect { fill: #0d1117; }
  .stripe { fill: #151b23; }
}
</style>
<rect width="${width}" height="${height}"/>
${caption ? `<text class="caption" x="${pad}" y="15">${escape(caption)}</text>` : ""}
${rows.map((_, index) => (index % 2 === 0 ? `<rect class="stripe" y="${top + rowHeight * (index + 1)}" width="${width}" height="${rowHeight}"/>` : "")).join("")}
${cells(header, top, "head")}
${rows.map((row, index) => cells(row, top + rowHeight * (index + 1), "cell")).join("\n")}
</svg>
`

await writeFile(output, svg)
