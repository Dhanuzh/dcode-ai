import { useState } from 'react'

type Props = {
  command: string
  /** Shell prompt character shown before the command. */
  prompt?: string
  className?: string
}

/** A command line with a copy-to-clipboard affordance. */
export function CopyCommand({ command, prompt = '$', className = '' }: Props) {
  const [copied, setCopied] = useState(false)

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1600)
    } catch {
      // Clipboard can be blocked (insecure context / permissions); the text
      // stays selectable, so failing quietly is the right fallback here.
    }
  }

  return (
    <div
      className={`group flex items-center gap-3 rounded-lg border border-line bg-surface px-4 py-3 ${className}`}
    >
      <span aria-hidden className="select-none font-mono text-sm text-accent">
        {prompt}
      </span>
      <code className="flex-1 overflow-x-auto whitespace-nowrap font-mono text-sm text-fg">
        {command}
      </code>
      <button
        type="button"
        onClick={copy}
        aria-label={copied ? 'Copied' : 'Copy command'}
        className="shrink-0 rounded-md border border-line px-2.5 py-1 font-mono text-xs text-muted transition hover:border-accent hover:text-accent focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
      >
        {copied ? '✓ copied' : 'copy'}
      </button>
    </div>
  )
}
