import { isMatching, P } from "ts-pattern"

const postPattern = {
  title: P.string,
  content: P.string,
  stars: P.number.between(1, 5).optional(),
  author: {
    firstName: P.string,
    lastName: P.string.optional(),
    followerCount: P.number,
  },
} as const

type Post = P.infer<typeof postPattern>

type Response = { data: Post[] } | { error: string }

export const inputs = [
  {
    data: [
      {
        title: "TS-Pattern",
        content: "Pattern matching",
        stars: 5,
        author: {
          firstName: "Gabriel",
          lastName: "Vergnaud",
          followerCount: 1000,
        },
      },
    ],
  },
  { error: "bad response" },
] satisfies Response[]

export const run = (response: Response): number =>
  isMatching({ data: P.array(postPattern) }, response) ? response.data.length : 0
