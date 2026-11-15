import { match, P } from "ts-pattern"

export type Input = { type: "post"; user: { name: string } } | { type: "comment"; text: string }

export const inputs = [
  { type: "post", user: { name: "Gabriel" } },
  { type: "comment", text: "Hi" },
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with({ type: "post", user: { name: P.select() } }, (username) => username)
    .otherwise(() => "anonymous")
