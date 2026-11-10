import { useEffect, useRef, useState } from 'react'

/**
 * A notebook rendered into an `<iframe sandbox="allow-scripts">` (#683, #684).
 *
 * The sandbox omits the same-origin permission: the notebook's HTML, images and scripts can
 * run, but they reach neither Nova's DOM nor its API. The document reports its own height by
 * `postMessage`, and only a message from this frame's window is believed.
 */
export default function NotebookFrame({ html, title }: { html: string; title: string }) {
  const [height, setHeight] = useState(600)
  const frameRef = useRef<HTMLIFrameElement | null>(null)

  useEffect(() => {
    function onMessage(event: MessageEvent) {
      if (event.source !== frameRef.current?.contentWindow) return
      const reported = (event.data as { nbHeight?: unknown } | null)?.nbHeight
      if (typeof reported !== 'number' || !Number.isFinite(reported)) return
      setHeight(Math.min(Math.round(reported), 20_000))
    }
    window.addEventListener('message', onMessage)
    return () => window.removeEventListener('message', onMessage)
  }, [])

  return (
    <iframe
      ref={frameRef}
      title={title}
      sandbox="allow-scripts"
      srcDoc={html}
      className="w-full rounded-[10px] border border-border bg-white"
      style={{ height }}
    />
  )
}
