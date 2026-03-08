import { createContext, type JSX } from 'solid-js'

export type ToolBlockContextValue = {
  setExtraLabel: (label: JSX.Element) => void
}

export const ToolBlockContext = createContext<ToolBlockContextValue>()
