// Turn duration marker shown between assistant turns. Top-level DisplayItem kind='system'.

import type { DisplayableEvent, SystemEvent } from '../../lib/types'
import mb from './MessageBlock.module.css'

export default function SystemMessageView(props: {
  msg: DisplayableEvent
}) {
  const sys = () => props.msg as SystemEvent
  return (
    <div class={`${mb.message} ${mb.system}`} data-role="system">
      Turn completed in{' '}
      {(sys().durationMs! / 1000).toFixed(1)}s
    </div>
  )
}
