export interface GrepLine {
  filePath: string | null
  lineNum: number
  content: string
  isMatch: boolean
}

export interface GrepGroup {
  filePath: string | null
  lines: GrepLine[]
}

/**
 * Parse ripgrep output into structured groups.
 *
 * Match lines use `:` separators:   filepath:linenum:content
 * Context lines use `-` separators: filepath-linenum-content
 * Section breaks are `--` on their own line.
 * Sometimes filenames are omitted:  linenum:content / linenum-content
 */
export function parseGrepOutput(raw: string): GrepGroup[] {
  const lines = raw.split('\n')
  const groups: GrepGroup[] = []
  let current: GrepGroup | null = null

  for (const line of lines) {
    if (line === '--') {
      // section break - finalize current group
      if (current && current.lines.length > 0) {
        groups.push(current)
        current = null
      }
      continue
    }

    if (line === '') continue

    const parsed = parseLine(line)
    if (!parsed) continue

    // Start new group when file changes (normalize relative vs absolute)
    if (!current || !sameFile(current.filePath, parsed.filePath)) {
      if (current && current.lines.length > 0) {
        groups.push(current)
      }
      // Prefer the longer (absolute) path as the canonical name
      current = { filePath: parsed.filePath, lines: [] }
    } else if (
      parsed.filePath &&
      current.filePath &&
      parsed.filePath.length > current.filePath.length
    ) {
      current.filePath = parsed.filePath
    }

    current.lines.push(parsed)
  }

  if (current && current.lines.length > 0) {
    groups.push(current)
  }

  return groups
}

/** Check if two file paths refer to the same file.
 *  Handles relative vs absolute: if one path ends with the other, they match. */
function sameFile(a: string | null, b: string | null): boolean {
  if (a === b) return true
  if (a === null || b === null) return false
  // One must be a path-suffix of the other (match at `/` boundary)
  const [longer, shorter] = a.length >= b.length ? [a, b] : [b, a]
  return longer.endsWith(shorter) && longer[longer.length - shorter.length - 1] === '/'
}

// Try no-file patterns first (unambiguous), then file patterns.
// For file patterns, the separator is always followed by a line number (digits).
// File paths may contain `:` or `-`, so we match the first :digits: or
// the pattern where `-` is followed by digits then `-`.

// No-file match: starts with digits then `:`
const NOFILE_MATCH = /^(\d+):(.*)$/s
// No-file context: starts with digits then `-`
const NOFILE_CONTEXT = /^(\d+)-(.*)$/s

function parseLine(line: string): GrepLine | null {
  // Try no-file patterns first
  let m = NOFILE_MATCH.exec(line)
  if (m && parseInt(m[1]) > 0) {
    return { filePath: null, lineNum: parseInt(m[1], 10), content: m[2], isMatch: true }
  }

  m = NOFILE_CONTEXT.exec(line)
  if (m && parseInt(m[1]) > 0) {
    return { filePath: null, lineNum: parseInt(m[1]), content: m[2], isMatch: false }
  }

  // File patterns: find :digits: for match or -digits- for context
  // Match line: scan for first `:digits:` that could be a line number
  const matchIdx = findSeparator(line, ':')
  if (matchIdx !== null) {
    return {
      filePath: line.slice(0, matchIdx.pathEnd),
      lineNum: matchIdx.lineNum,
      content: line.slice(matchIdx.contentStart),
      isMatch: true,
    }
  }

  // Context line: scan for `-digits-`
  const contextIdx = findSeparator(line, '-')
  if (contextIdx !== null) {
    return {
      filePath: line.slice(0, contextIdx.pathEnd),
      lineNum: contextIdx.lineNum,
      content: line.slice(contextIdx.contentStart),
      isMatch: false,
    }
  }

  // Unparseable - treat as a match line with no metadata
  return { filePath: null, lineNum: 0, content: line, isMatch: true }
}

function findSeparator(
  line: string,
  sep: string,
): { pathEnd: number; lineNum: number; contentStart: number } | null {
  // Look for `sep + digits + sep` pattern.
  // For paths starting with `/`, start scanning after position 1.
  let i = 1
  while (i < line.length) {
    const sepIdx = line.indexOf(sep, i)
    if (sepIdx === -1) return null

    // Check if what follows is digits then separator
    let j = sepIdx + 1
    while (j < line.length && line[j] >= '0' && line[j] <= '9') j++

    if (j > sepIdx + 1 && j < line.length && line[j] === sep) {
      const lineNum = parseInt(line.slice(sepIdx + 1, j), 10)
      if (lineNum > 0) {
        return {
          pathEnd: sepIdx,
          lineNum,
          contentStart: j + 1,
        }
      }
    }

    i = sepIdx + 1
  }

  return null
}
