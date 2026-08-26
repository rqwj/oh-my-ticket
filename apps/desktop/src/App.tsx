/**
 * U8 scaffold shell (KD4): a fresh desktop surface with NO reuse of the
 * DSH plugin's components. Real UI (tree/details/runs/settings) is unit
 * U10; this shell only proves the dual dev/build path renders.
 */
export function App() {
  return (
    <main style={{ fontFamily: 'system-ui, sans-serif', padding: 24 }}>
      <h1>oh-my-ticket desktop</h1>
      <p>脚手架已就绪 — daemon 状态将在 U9 接入后显示于此。</p>
    </main>
  )
}
