/**
 * Local IPC transport for the OMT daemon (plan U5a): JSON-RPC 2.0 over a
 * unix domain socket (POSIX) or a named pipe (Windows), carried on
 * `net.Socket` in both cases.
 *
 * Framing (pinned, mirrors crates/omt-runtime/README.md): NEWLINE-DELIMITED
 * JSON — exactly one JSON-RPC message object per line, `\n` terminated,
 * UTF-8. No batch arrays, no content-length headers. The server interleaves
 * responses and `omt/event` notifications over the same ordered connection.
 *
 * Credentials travel ONLY inside request params; this transport never logs
 * lines or error objects wholesale (redaction happens server-side, and we
 * keep client-side logging out of the library entirely).
 */
import { Socket } from 'node:net'

export interface JsonRpcErrorShape {
  code: number
  message: string
  /** OMT Problem: {code, details, message} (R5). */
  data?: ProblemShape
}

export interface ProblemShape {
  code: string
  details?: unknown
  message?: string
}

/** Error thrown by {@link Transport.call} when the daemon answers a Problem. */
export class OmtProtocolError extends Error {
  readonly problemCode: string
  readonly details: unknown

  constructor(problem: ProblemShape) {
    super(problem.message ?? problem.code)
    this.name = 'OmtProtocolError'
    this.problemCode = problem.code
    this.details = problem.details ?? null
  }
}

type ResponseHandler = (value: unknown) => void

interface PendingRequest {
  resolve: (value: unknown) => void
  reject: (error: Error) => void
  timer: NodeJS.Timeout | undefined
}

export interface TransportOptions {
  /** Per-request timeout in ms. Default 30_000. */
  requestTimeoutMs?: number
  /** Called for every server-pushed notification (`omt/event`). */
  onNotification?: (method: string, params: unknown) => void
  /** Called once when the socket closes (after connect). */
  onClose?: () => void
}

/**
 * One newline-delimited JSON-RPC 2.0 connection to omt-daemon.
 */
export class Transport {
  private readonly socket: Socket
  private readonly pending = new Map<unknown, PendingRequest>()
  private nextId = 1
  private buffer = ''
  private connected = false
  private readonly requestTimeoutMs: number
  private readonly onNotification: (method: string, params: unknown) => void
  private readonly onClose?: () => void
  private closeHandlers: Array<() => void> = []

  private constructor(socket: Socket, options: TransportOptions = {}) {
    this.socket = socket
    this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000
    this.onNotification =
      options.onNotification ??
      (() => {
        /* notifications without a listener are dropped */
      })
    this.onClose = options.onClose

    let sawConnect = false
    socket.on('connect', () => {
      sawConnect = true
      this.connected = true
      this.drainWaiters()
    })
    socket.setEncoding('utf8')
    socket.on('data', (chunk: string) => this.consume(chunk))
    socket.on('error', (err: Error) => this.failAll(err))
    socket.on('close', () => {
      const wasConnected = this.connected
      this.connected = false
      this.failAll(new Error('connection closed'))
      if (wasConnected || sawConnect) {
        this.closeHandlers.forEach((handler) => handler())
        this.onClose?.()
      }
    })
  }

  /**
   * Connect over a unix domain socket path (posix) or named pipe name
   * (`\\\\.\\pipe\\...`, windows). Resolves when the kernel completes the
   * local connection.
   */
  static connect(endpoint: string, options: TransportOptions = {}): Promise<Transport> {
    return new Promise((resolve, reject) => {
      const socket = new Socket()
      const transport = new Transport(socket, options)
      const timer = setTimeout(() => {
        socket.destroy()
        reject(new Error(`connect timeout: ${endpoint}`))
      }, options.requestTimeoutMs ?? 10_000)
      socket.once('connect', () => {
        clearTimeout(timer)
        resolve(transport)
      })
      socket.once('error', (err: Error) => {
        clearTimeout(timer)
        reject(err)
      })
      // UDS paths and pipe names both go through net.Socket.connect.
      socket.connect(endpoint)
    })
  }

  /** Raw socket handle for tests and advanced callers. */
  get raw(): Socket {
    return this.socket
  }

  get isConnected(): boolean {
    return this.connected && !this.socket.destroyed
  }

  private waiters: Array<() => void> = []
  private drainWaiters(): void {
    const list = this.waiters.splice(0)
    list.forEach((wake) => wake())
  }

  /** Resolves once the socket is connected (immediately when already up). */
  whenConnected(): Promise<void> {
    if (this.isConnected) return Promise.resolve()
    return new Promise((resolve) => {
      this.waiters.push(resolve)
    })
  }

  /**
   * Send one JSON-RPC request and await its response. Rejects with
   * {@link OmtProtocolError} when the daemon answers with a Problem.
   *
   * `hooks.onIssued` receives the wire id as soon as the request line is
   * written, enabling `$/cancelRequest` cancellation via {@link sendCancel}
   * (U5c: the server honors cancellation only at linearization-safe points;
   * after the op is durable the call completes normally).
   */
  call(
    method: string,
    params: unknown = {},
    hooks?: { onIssued?: (id: number) => void },
  ): Promise<unknown> {
    if (!this.isConnected) {
      return Promise.reject(new Error('transport not connected'))
    }
    const id = this.nextId++
    const line = JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n'
    hooks?.onIssued?.(id)
    return new Promise((resolve, reject) => {
      const timer =
        this.requestTimeoutMs > 0
          ? setTimeout(() => {
              this.pending.delete(id)
              reject(new Error(`request timeout: ${method}`))
            }, this.requestTimeoutMs)
          : undefined
      this.pending.set(id, { resolve, reject, timer })
      this.socket.write(line, (err) => {
        if (err) {
          this.pending.delete(id)
          if (timer !== undefined) clearTimeout(timer)
          reject(err)
        }
      })
    })
  }

  /**
   * Send the JSON-RPC cancellation notification for an in-flight call id
   * (U5c): `{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id"}}`.
   * Fire-and-forget by protocol design — the response to the ORIGINAL
   * request settles the promise (CANCELED problem or the completed result).
   */
  sendCancel(id: number): void {
    if (!this.isConnected) return
    try {
      const line =
        JSON.stringify({
          jsonrpc: '2.0',
          method: '$/cancelRequest',
          params: { id },
        }) + '\n'
      this.socket.write(line)
    } catch {
      /* socket raced shutdown: nothing to cancel anyway */
    }
  }

  /** Register an extra close handler (idempotent removal on unsubscribe). */
  addCloseHandler(handler: () => void): void {
    this.closeHandlers.push(handler)
  }

  /** Half-close the write side, then destroy. In-flight requests reject. */
  end(): void {
    this.connected = false
    try {
      this.socket.end()
    } catch {
      /* already gone */
    }
    this.socket.destroy()
  }

  private consume(chunk: string): void {
    this.buffer += chunk
    let newlineAt = this.buffer.indexOf('\n')
    while (newlineAt >= 0) {
      const line = this.buffer.slice(0, newlineAt).trim()
      this.buffer = this.buffer.slice(newlineAt + 1)
      if (line.length > 0) this.dispatchLine(line)
      newlineAt = this.buffer.indexOf('\n')
    }
    // Unbounded residue guard: drop absurd partial lines (>16 MiB).
    if (this.buffer.length > 16 * 1024 * 1024) {
      this.buffer = ''
      this.failAll(new Error('framing desync: oversized partial line'))
    }
  }

  private dispatchLine(line: string): void {
    let value: unknown
    try {
      value = JSON.parse(line)
    } catch {
      return // malformed line: ignored (server guarantees well-formed output)
    }
    const record = value as { id?: unknown; method?: unknown; error?: JsonRpcErrorShape; result?: unknown }
    if (typeof record.method === 'string' && record.id === undefined) {
      this.onNotification(record.method, (value as { params?: unknown }).params ?? null)
      return
    }
    const pending = record.id === null ? undefined : this.pending.get(record.id)
    if (!pending) return // late answer after timeout: dropped
    this.pending.delete(record.id)
    if (pending.timer !== undefined) clearTimeout(pending.timer)
    if (record.error) {
      pending.reject(
        new OmtProtocolError(record.error.data ?? { code: 'UNKNOWN', details: record.error }),
      )
    } else {
      pending.resolve(record.result)
    }
  }

  private failAll(error: Error): void {
    const entries = [...this.pending.values()]
    this.pending.clear()
    entries.forEach((pending) => {
      if (pending.timer !== undefined) clearTimeout(pending.timer)
      pending.reject(error)
    })
  }
}
