// User text message rendered as markdown. Top-level DisplayItem kind='user'.

import type { DisplayableEvent } from '../../lib/types'
import MessageBlock from './MessageBlock'
import mb from './MessageBlock.module.css'
import Prose from '../Prose'

export default function UserMessageView(props: {
  msg: DisplayableEvent
  sessionId: string
}) {
  const text = () => {
    if (props.msg.type !== 'user') return ''
    return typeof props.msg.message.content === 'string' ? props.msg.message.content : ''
  }
  return (
    <MessageBlock variant="user" role="user" label="User" meta={{ sessionId: props.sessionId, uuid: props.msg.uuid }}>
      <Prose
        text={text()}
        class={mb.content}
      />
    </MessageBlock>
  )
}
