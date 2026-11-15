import { isMatching, P } from "ts-pattern"

type User = { name: string; age: number }

const userPattern: P.Pattern<User> = {
  name: "Alice",
}

export const inputs = [
  { name: "Alice", age: 42 },
  { name: "Bob", age: 30 },
] satisfies User[]

export const run = (input: User): string => (isMatching(userPattern, input) ? "match" : "miss")
