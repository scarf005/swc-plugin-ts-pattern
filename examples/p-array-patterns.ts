import { match, P } from "ts-pattern"

export type Input = { title: string; content: string }[]

export const inputs = [
  [
    { title: "Hello world!", content: "This is a very interesting content" },
    { title: "Bonjour!", content: "This is a very interesting content too" },
  ],
  [],
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with(P.array({ title: P.string, content: P.string }), () => "a list of posts!")
    .otherwise(() => "something else")
