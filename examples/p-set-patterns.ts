import { match, P } from "ts-pattern"

export type Input = Set<string | number>

export const inputs = [new Set([1, 2, 3]), new Set(["a", "b"])] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with(P.set(1), () => "Set contains only 1")
    .with(P.set(P.string), () => "Set contains only strings")
    .with(P.set(P.number), () => "Set contains only numbers")
    .otherwise(() => "")
