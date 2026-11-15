import { match, P } from "ts-pattern"

export type User = { age: number; name: string }
export type Post = { body: string }
export type Input = { author: User; content: Post }

export const inputs = [
  { author: { age: 25, name: "Gabriel" }, content: { body: "Hello" } },
  { author: { age: 16, name: "Guest" }, content: { body: "Hidden" } },
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with({ author: P.select({ age: P.number.gt(18) }) }, (author) => author.name)
    .with(
      {
        author: P.select("author", { age: P.number.gt(18) }),
        content: P.select("content"),
      },
      ({ author, content }) => `${author.name}:${content.body}`,
    )
    .otherwise(() => "anonymous")
