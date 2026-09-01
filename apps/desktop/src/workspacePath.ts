/**
 * 添加 workspace 的路径解析：用户选的是「目录」，declare 需要的是 home
 * 目录（含 .omt 的那个）。规则：
 * - 选中路径本身以 .omt 结尾 → 直接使用；
 * - 否则按「工作区根目录」处理，取 <path>/.omt。
 * 存在性不做前端预探：daemon 的 declare 打开前校验（R9）会结构化拒绝
 * 缺失路径，错误文案在设置页映射为用户指引。
 */
export function resolveHomeFromPickedDir(picked: string): string {
  const trimmed = picked.replace(/\/+$/, '')
  if (trimmed.endsWith('/.omt') || trimmed === '.omt') return trimmed
  return `${trimmed}/.omt`
}

export const NOT_A_WORKSPACE_HINT = '所选目录还不是 omt 工作区（其中没有 .omt 目录）。请先在 DSH 插件或 CLI 中初始化，或改选其他目录。'
