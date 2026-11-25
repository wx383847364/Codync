// Writes resources/account-config.json from apps/shared/Config/<env>.plist: the Clerk publishable
// key and the cloud URL the build talks to. Usage: node tools/account-config.mjs dev|main
// (the `npm run dev` / `dist:*:<env>` scripts run it).
import { spawnSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const env = process.argv[2]
if (!['dev', 'main'].includes(env)) throw new Error(`unknown environment ${env}`)
const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const plist = readFileSync(join(root, '../shared/Config', `${env}.plist`), 'utf8')
const value = (key) => new RegExp(`<key>${key}</key>\\s*<string>([^<]*)</string>`).exec(plist)?.[1] ?? null
// 记录实际被打包的 Git 提交，避免同版本号的构建无法区分。
const git = { cwd: root, encoding: 'utf8' }
const head = spawnSync('git', ['rev-parse', '--short=12', 'HEAD'], git)
const commit = head.status === 0 ? head.stdout.trim() : null
const dirty = commit && spawnSync('git', ['status', '--porcelain', '--untracked-files=no'], git)
const buildCommit = dirty && dirty.status === 0 && dirty.stdout.trim() ? `${commit}+` : commit
writeFileSync(
  join(root, 'resources/account-config.json'),
  `${JSON.stringify({ environment: env, buildCommit, clerkPublishableKey: value('clerkPublishableKey'), cloudURL: value('cloudURL') }, null, 2)}\n`,
)
console.log(`account config: ${env}`)
