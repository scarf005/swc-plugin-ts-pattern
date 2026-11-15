import { match, P } from "ts-pattern"

type Input = ["a" | "b" | "c", "a" | "b" | "c"]
const pattern = ["a", P.union("a", "b")] as const

type Narrowed = P.narrow<Input, typeof pattern>

export const inputs = [
  ["a", "a"],
  ["a", "b"],
  ["c", "c"],
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with(pattern, (value: Narrowed) => `narrowed:${value[0]}${value[1]}`)
    .otherwise(([left, right]) => `other:${left}${right}`)
