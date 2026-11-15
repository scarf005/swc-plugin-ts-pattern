import { match, P } from "ts-pattern"

export type Input = Map<string, string | number>

export const inputs = [
  new Map([
    ["a", 1],
    ["b", 2],
    ["c", 3],
  ]),
  new Map([["a", "x"]]),
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with(P.map(P.string, P.number), () => "map's type is Map<string, number>")
    .with(P.map(P.string, P.string), () => "map's type is Map<string, string>")
    .with(P.map(P.union("a", "c"), P.number), () => "map's type is Map<'a' | 'c', number>")
    .otherwise(() => "")
