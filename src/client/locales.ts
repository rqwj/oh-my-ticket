/**
 * OMT browser dictionaries. Single namespace 'omt', registered with the
 * shell locale service at apply time; slot entries declare `locale: NS` and
 * the render machinery synthesizes the `t` seat on component props. Dynamic
 * copy (ids, titles, counts) interpolates {placeholder} params.
 */

export const NS = 'omt'

/** Simplified Chinese OMT UI messages. */
export const zh = {
  'status.open': '未开始',
  'status.inProgress': '进行中',
  'status.done': '已完成',
  'status.archived': '已归档',
  'status.archivedWith': '已归档（{status}）',

  'priority.p0': '普通',
  'priority.p1': '关注',
  'priority.p2': '重要',
  'priority.p3': '紧急',
  'priority.iconTitle': '优先级 P{priority} {label}',
  'priority.selectTitle': '优先级',

  'time.justNow': '刚刚',
  'time.minutesAgo': '{count} 分钟前',
  'time.hoursAgo': '{count} 小时前',
  'time.daysAgo': '{count} 天前',
  'time.localeTag': 'zh-CN',

  'node.titleWithStatus': '{id} {title}（{status}）',

  'drawer.aria': 'OMT ticket 树',
  'drawer.refresh': '刷新',
  'drawer.collapse': '收起',
  'drawer.searchPlaceholder': '搜索 id / 标题…',
  'drawer.filterType': '按类型过滤：{type}',
  'drawer.filterStatus': '按状态过滤：{status}',
  'drawer.filterPriority': '按优先级过滤：P{priority}',
  'drawer.showArchived': '📦 已归档',
  'drawer.showArchivedTitle': '显示已归档的节点',
  'drawer.showId': '# 编号',
  'drawer.showIdTitle': '在树行中显示节点编号',
  'drawer.sortNone': '不排序',
  'drawer.sortPriorityDesc': '优先级降序',
  'drawer.sortPriorityAsc': '优先级升序',
  'drawer.loading': '加载中…',
  'drawer.loadFailed': '加载失败：{message}',
  'drawer.empty': '还没有 ticket。让模型用 omt_create 创建一个吧。',
  'drawer.noMatch': '没有匹配的节点。',
  'drawer.dragHandle': '拖拽调整宽度',
  'drawer.reindex': '重建',
  'drawer.reindexConfirm': '确认？',
  'drawer.reindexTitle': '从磁盘重建索引',
  'drawer.reindexArmedTitle': '再次点击确认重建索引',
  'drawer.archiveTitle': '归档 {id}',

  'panel.toFloat': '以浮窗显示',
  'panel.toDrawer': '以抽屉显示',
  'panel.popOut': '弹出为浮窗',
  'float.aria': 'OMT ticket 浮窗',
  'float.move': '拖拽移动',
  'float.resize': '拖拽调整大小',
  'tab.aria': 'OMT ticket 面板',

  'doc.close': '关闭',
  'doc.notFound': '节点不存在或已被移除',
  'doc.loadFailed': '加载失败',
  'doc.retry': '重试',
  'doc.forget': '从激活/相关中移除',
  'doc.statusReadonly': '已归档节点只读，恢复后可调整状态',
  'doc.editTitle': '点击编辑标题',
  'doc.createdAt': '创建于 {time}',
  'doc.updatedAt': '更新于 {time}',
  'doc.timesTitle': '创建: {created}\n更新: {updated}',
  'doc.running': '⟳ 执行中：{session} · 开始于 {since}',
  'doc.parent': '父节点',
  'doc.children': '子节点',
  'doc.execute': '执行',
  'doc.executeTitle': '在对话框中引用并提交执行',
  'doc.executeArchived': '已归档节点不可执行',
  'doc.locked': '执行中，完成后可操作',
  'doc.archive': '归档',
  'doc.restore': '恢复',
  'doc.copied': '已复制',
  'doc.copyId': '复制 id',
  'doc.copyPath': '复制路径',
  'doc.appendPlaceholder': '追加进度记录…',
  'doc.appendPlaceholderArchived': '已归档节点只读，恢复后可追加',
  'doc.appendPlaceholderLocked': '执行中，完成后可追加',
  'doc.append': '追加',
  'doc.executeDraft': '开始执行这个 ticket',

  'dock.openInPanel': '在详情面板中打开',
  'dock.clear': '取消激活',

  'refs.aria': '引用的 ticket',
  'refs.label': '引用',

  'turn.aria': '相关 ticket',
  'turn.label': '相关 ticket',

  'toggle.collapse': '收起 OMT ticket 树',
  'toggle.expand': '展开 OMT ticket 树',

  'show.loading': '查询 {id} 中…',
  'show.empty': '（无内容）',

  'serialize.failed': '无法序列化 ticket 引用 {ref}: {message}',
  'serialize.parentLine': '父节点: {id} {title}',
  'serialize.parentRoot': '父节点: （根节点）',
  'serialize.childrenLine': '子节点: {list}',
  'serialize.childrenNone': '子节点: （无）',
  'serialize.childSep': '、',

  'doc.breadcrumb': '祖先',
  'doc.editBody': '编辑正文',
  'doc.saveBody': '保存正文',
  'doc.cancelEdit': '取消',
  'drawer.newEpic': '新建 Epic',
  'drawer.addChild': '新建子节点',
  'drawer.createTitle': '标题',
  'drawer.createBody': '正文（可选）',
  'drawer.createSubmit': '创建',
  'drawer.createCancel': '取消',
  'drawer.locate': '定位激活节点',
  'settings.nav': 'OMT',
  'settings.title': 'OMT 约定',
  'settings.helper': '在这里写追加约定并勾选拆票/实施 skill。默认 OMT 规范始终生效。',
  'settings.extraLabel': '追加约定',
  'settings.extraHelper': '这段文字会叠在内置 OMT 规范后面。留空则只用默认规范。',
  'settings.extraPlaceholder': '例如：拆票标题用中文',
  'settings.bindLabel': '绑定 skill',
  'settings.loading': '加载 skill 列表…',
  'settings.empty': '没有可绑定的 skill。',
  'settings.loadFailed': '加载失败：{message}',
  'settings.retry': '重试',
  'settings.missing': '已卸载',
  'settings.omtHint': 'OMT 规范已默认在场，勾选可选',
  'settings.writeFailed': '保存失败：{message}',
} satisfies Record<string, string>

/** Translation keys owned by the OMT UI namespace. */
export type OmtKey = keyof typeof zh

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface LocaleNamespaceMap {
    /** OMT plugin UI copy. */
    omt: OmtKey
  }
}

/**
 * The framework-injected `t` seat, structurally typed for this out-of-tree
 * package (shapes copied from '@deepseek-ai/dsh-client-ui-slots' Translate).
 */
export type Translate = (key: OmtKey, params?: Record<string, unknown>) => string

/** English OMT UI messages. */
export const en: Record<OmtKey, string> = {
  'status.open': 'Open',
  'status.inProgress': 'In progress',
  'status.done': 'Done',
  'status.archived': 'Archived',
  'status.archivedWith': 'Archived ({status})',

  'priority.p0': 'Normal',
  'priority.p1': 'Watch',
  'priority.p2': 'Important',
  'priority.p3': 'Urgent',
  'priority.iconTitle': 'Priority P{priority} {label}',
  'priority.selectTitle': 'Priority',

  'time.justNow': 'just now',
  'time.minutesAgo': '{count} min ago',
  'time.hoursAgo': '{count} h ago',
  'time.daysAgo': '{count} d ago',
  'time.localeTag': 'en-US',

  'node.titleWithStatus': '{id} {title} ({status})',

  'drawer.aria': 'OMT ticket tree',
  'drawer.refresh': 'Refresh',
  'drawer.collapse': 'Collapse',
  'drawer.searchPlaceholder': 'Search id / title…',
  'drawer.filterType': 'Filter by type: {type}',
  'drawer.filterStatus': 'Filter by status: {status}',
  'drawer.filterPriority': 'Filter by priority: P{priority}',
  'drawer.showArchived': '📦 Archived',
  'drawer.showArchivedTitle': 'Show archived nodes',
  'drawer.showId': '# ID',
  'drawer.showIdTitle': 'Show node IDs in tree rows',
  'drawer.sortNone': 'No sorting',
  'drawer.sortPriorityDesc': 'Priority descending',
  'drawer.sortPriorityAsc': 'Priority ascending',
  'drawer.loading': 'Loading…',
  'drawer.loadFailed': 'Failed to load: {message}',
  'drawer.empty': 'No tickets yet. Ask the model to create one with omt_create.',
  'drawer.noMatch': 'No matching nodes.',
  'drawer.dragHandle': 'Drag to resize',
  'drawer.reindex': 'Reindex',
  'drawer.reindexConfirm': 'Confirm?',
  'drawer.reindexTitle': 'Rebuild the index from disk',
  'drawer.reindexArmedTitle': 'Click again to confirm reindex',
  'drawer.archiveTitle': 'Archive {id}',

  'panel.toFloat': 'Show as a floating window',
  'panel.toDrawer': 'Show as the drawer',
  'panel.popOut': 'Pop out as a floating window',
  'float.aria': 'OMT ticket floating window',
  'float.move': 'Drag to move',
  'float.resize': 'Drag to resize',
  'tab.aria': 'OMT ticket panel',

  'doc.close': 'Close',
  'doc.notFound': 'This node no longer exists',
  'doc.loadFailed': 'Failed to load',
  'doc.retry': 'Retry',
  'doc.forget': 'Remove from active/related',
  'doc.statusReadonly': 'Archived nodes are read-only; restore to change status',
  'doc.editTitle': 'Click to edit the title',
  'doc.createdAt': 'Created {time}',
  'doc.updatedAt': 'Updated {time}',
  'doc.timesTitle': 'Created: {created}\nUpdated: {updated}',
  'doc.running': '⟳ Running: {session} · started {since}',
  'doc.parent': 'Parent',
  'doc.children': 'Children',
  'doc.execute': 'Execute',
  'doc.executeTitle': 'Reference in the composer and submit to execute',
  'doc.executeArchived': 'Archived nodes cannot be executed',
  'doc.locked': 'Running; available when it finishes',
  'doc.archive': 'Archive',
  'doc.restore': 'Restore',
  'doc.copied': 'Copied',
  'doc.copyId': 'Copy ID',
  'doc.copyPath': 'Copy path',
  'doc.appendPlaceholder': 'Add a progress note…',
  'doc.appendPlaceholderArchived': 'Archived nodes are read-only; restore to append',
  'doc.appendPlaceholderLocked': 'Running; append when it finishes',
  'doc.append': 'Append',
  'doc.executeDraft': 'Start executing this ticket',

  'dock.openInPanel': 'Open in the details panel',
  'dock.clear': 'Deactivate',

  'refs.aria': 'Referenced tickets',
  'refs.label': 'Refs',

  'turn.aria': 'Related tickets',
  'turn.label': 'Related tickets',

  'toggle.collapse': 'Collapse the OMT ticket tree',
  'toggle.expand': 'Expand the OMT ticket tree',

  'show.loading': 'Loading {id}…',
  'show.empty': '(No content)',

  'serialize.failed': 'Failed to serialize ticket reference {ref}: {message}',
  'serialize.parentLine': 'Parent: {id} {title}',
  'serialize.parentRoot': 'Parent: (root)',
  'serialize.childrenLine': 'Children: {list}',
  'serialize.childrenNone': 'Children: (none)',
  'serialize.childSep': ', ',

  'doc.breadcrumb': 'Ancestors',
  'doc.editBody': 'Edit body',
  'doc.saveBody': 'Save body',
  'doc.cancelEdit': 'Cancel',
  'drawer.newEpic': 'New Epic',
  'drawer.addChild': 'New child',
  'drawer.createTitle': 'Title',
  'drawer.createBody': 'Body (optional)',
  'drawer.createSubmit': 'Create',
  'drawer.createCancel': 'Cancel',
  'drawer.locate': 'Locate active node',
  'settings.nav': 'OMT',
  'settings.title': 'OMT rules',
  'settings.helper': 'Add extra rules and bind split/implement skills here. Default OMT rules are always on.',
  'settings.extraLabel': 'Extra rules',
  'settings.extraHelper': 'This text is appended after the built-in OMT spec. Leave empty to keep the defaults.',
  'settings.extraPlaceholder': 'Example: use Chinese ticket titles',
  'settings.bindLabel': 'Bound skills',
  'settings.loading': 'Loading skills…',
  'settings.empty': 'No bindable skills.',
  'settings.loadFailed': 'Failed to load: {message}',
  'settings.retry': 'Retry',
  'settings.missing': 'Uninstalled',
  'settings.omtHint': 'OMT rules are already always on; binding is optional',
  'settings.writeFailed': 'Save failed: {message}',
}

/** Status value → dictionary key (shared by every status label site). */
export const STATUS_KEY = {
  open: 'status.open',
  in_progress: 'status.inProgress',
  done: 'status.done',
} as const satisfies Record<string, OmtKey>
