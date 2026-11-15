import { match, P } from "ts-pattern"

export type Input = { score: number }

export const inputs = [
  { score: 10 },
  { score: 5 },
  {
    score: 2,
  },
] satisfies Input[]

export const run = (input: Input): string =>
  match<Input>(input)
    .with({ score: P.when((score): score is 5 => score === 5) }, () => "😐")
    .with({ score: P.when((score) => score < 5) }, () => "😞")
    .otherwise(() => "🙂")
