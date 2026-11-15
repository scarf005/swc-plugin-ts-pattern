import { match, P } from "ts-pattern"

export type Input =
  | [number, "+", number]
  | [number, "-", number]
  | [number, "*", number]
  | ["-", number]

export const inputs = [
  [3, "*", 4],
  [1, "+", 2],
  [8, "-", 5],
  ["-", 7],
] satisfies Input[]

export const run = (input: Input): number =>
  match(input)
    .with([P._, "+", P._], ([x, , y]) => x + y)
    .with([P._, "-", P._], ([x, , y]) => x - y)
    .with([P._, "*", P._], ([x, , y]) => x * y)
    .with(["-", P._], ([, x]) => -x)
    .exhaustive()
