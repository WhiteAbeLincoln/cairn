export function* enumerate<T>(
  iterable: Iterable<T>,
  startIndex = 0,
): Generator<[number, T], void, unknown> {
  let index = startIndex
  for (const item of iterable) {
    yield [index, item]
    index++
  }
}

export function upperFirst(str: string): string {
  if (str.length === 0) return str
  return str[0].toUpperCase() + str.slice(1)
}

export type ValuesOf<O extends Record<keyof any, any>> = O[keyof O]
