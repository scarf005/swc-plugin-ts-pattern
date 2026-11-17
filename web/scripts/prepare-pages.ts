import { copyFileSync, writeFileSync } from "node:fs"
import { dirname, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const dist = resolve(dirname(fileURLToPath(import.meta.url)), "../dist")
// Cloudflare Pages reads _redirects; GitHub Pages serves 404.html for /examples/<id>.
writeFileSync(resolve(dist, "_redirects"), "/* /index.html 200\n")
copyFileSync(resolve(dist, "index.html"), resolve(dist, "404.html"))
