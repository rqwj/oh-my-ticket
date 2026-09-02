/**
 * SSE route for OMT change notifications: GET /omt/events keeps the
 * connection open and pushes one `data:` frame per change-hub event. The
 * browser subscribes with EventSource (same origin, auto-reconnect).
 * U7a: the hub is fed by daemon event envelopes via the runtime service.
 */
import type { Context } from '@deepseek-ai/cordis'
import type { ChangeHub } from './service.ts'

/** Structural ctx.webServer face (route registration only). */
interface WebServerLike {
  register(route: {
    kind: 'exact' | 'prefix'
    path: string
    handler: (req: { on(event: 'close', fn: () => void): void }, res: {
      writeHead(status: number, headers: Record<string, string>): void
      write(chunk: string): void
      end(): void
    }) => void
  }): () => void
}

/** Register the Server-Sent Events route broadcasting hub changes. */
export function registerOmtEvents(ctx: Context, hub: ChangeHub): void {
  const webServer = (ctx as unknown as { webServer?: WebServerLike }).webServer
  if (webServer === undefined) {
    console.warn('[omt] webServer service unavailable — change push disabled')
    return
  }
  webServer.register({
    kind: 'exact',
    path: '/omt/events',
    handler(req, res) {
      res.writeHead(200, {
        'content-type': 'text/event-stream; charset=utf-8',
        'cache-control': 'no-cache',
        'connection': 'keep-alive',
      })
      res.write(': omt events\n\n')
      const unsubscribe = hub.subscribe(event => {
        res.write(`data: ${JSON.stringify(event)}\n\n`)
      })
      req.on('close', unsubscribe)
    },
  })
}
