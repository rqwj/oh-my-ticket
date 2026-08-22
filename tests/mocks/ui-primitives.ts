/**
 * Test double for the '@deepseek-ai/dsh-client-ui-primitives' platform
 * module (answered by the loader table in the browser; aliased here so
 * DocPanel component tests run under jsdom without the monorepo graph).
 */
import { createElement, type ComponentType } from 'react'

/** Markdown stub: renders the raw text so tests can query it. */
export const MarkdownText: ComponentType<{ text: string; streaming?: boolean }> = ({ text }) =>
  createElement('div', { 'data-markdown': true }, text)
