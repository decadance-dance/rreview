// The manifest's ids are the plugin's identity: Orca keys its settings and storage on the plugin
// id, so renaming the displayed name must leave both ids alone (#45).
import { assert, test } from 'orca/testing'
import { readFileSync } from 'node:fs'

const manifest = JSON.parse(readFileSync(new URL('../orca-plugin.json', import.meta.url), 'utf8'))

test('the plugin shows as Rreview and keeps the rreview ids', () => {
  assert.equal(manifest.id, 'rreview')
  assert.equal(manifest.name, 'Rreview')
  assert.deepEqual(manifest.contributes.panels.map((p: { id: string; title: string }) => [p.id, p.title]), [['rreview', 'Rreview']])
})
