import { match, P } from "ts-pattern"

export type Input = Record<string, number>

export const inputs = [{ alice: 100, bob: 85, charlie: 92 }, { one: 1 }] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with(P.record(P.string, P.number), () => "All user scores")
    .with(P.record(P.string, P.string), () => "All user names")
    .otherwise(() => "")
