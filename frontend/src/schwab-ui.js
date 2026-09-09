const replacements = [
  ['Longbridge OpenAPI', 'Schwab Market Data API'],
  ['Longbridge OAuth 2.0', 'Schwab OAuth 2.0'],
  ['Longbridge', 'Schwab'],
  ['行情权限包', '数据权限'],
  ['Trade API', 'Trading API'],
  ['Paper Orders', 'Order API'],
  ['美股期权实时行情需要相应 OPRA 权限。', '仅使用 Schwab Market Data。账户、余额、下单和撤单接口均未启用。'],
  ['使用浏览器完成授权，不需要在工作台输入 App Secret。Access Token 只驻留 Rust 进程内存，不写入浏览器存储或本地文件。', 'Schwab App Secret 由 Rust 后端环境变量提供，不写入浏览器。授权完成后使用本区的 OAuth 回调输入框完成连接。'],
  ['完成授权后此窗口会自动验证连接；不要关闭本地服务。', 'Schwab 要求 HTTPS Callback URL。授权后若本地回调页无法打开，复制地址栏中的完整回调 URL，并粘贴到本区 OAuth 回调输入框。'],
]

const oauthStyle = document.createElement('style')
oauthStyle.textContent = `
  .credential-form,
  .execution-panel,
  .paper-confirm-backdrop {
    display: none !important;
  }
  .schwab-oauth-complete {
    display: grid;
    gap: 8px;
    margin-top: 12px;
    padding: 12px;
    border: 1px solid rgba(112, 165, 255, 0.28);
    border-radius: 10px;
    background: rgba(112, 165, 255, 0.06);
  }
  .schwab-oauth-complete label {
    display: grid;
    gap: 6px;
  }
  .schwab-oauth-complete textarea {
    width: 100%;
    min-height: 72px;
    resize: vertical;
  }
  .schwab-oauth-complete button {
    justify-self: start;
  }
  .schwab-oauth-helper {
    color: #83909c;
    font-size: 12px;
    line-height: 1.5;
  }
  .schwab-oauth-result {
    min-height: 18px;
    font-size: 12px;
  }
`
document.head.appendChild(oauthStyle)

function relabel(root = document.body) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT)
  const nodes = []
  while (walker.nextNode()) nodes.push(walker.currentNode)
  nodes.forEach((node) => {
    let value = node.nodeValue
    replacements.forEach(([from, to]) => {
      value = value.replaceAll(from, to)
    })
    if (value !== node.nodeValue) node.nodeValue = value
  })
  document.querySelectorAll('[title],[placeholder]').forEach((element) => {
    for (const attribute of ['title', 'placeholder']) {
      let value = element.getAttribute(attribute)
      if (!value) continue
      replacements.forEach(([from, to]) => {
        value = value.replaceAll(from, to)
      })
      element.setAttribute(attribute, value)
    }
  })
}

function ensureOAuthCompletion() {
  const section = document.querySelector('.oauth-section')
  if (!section || section.querySelector('.schwab-oauth-complete')) return

  const authorizationLink = section.querySelector('a.oauth-link')
  if (!authorizationLink) return

  const form = document.createElement('form')
  form.className = 'schwab-oauth-complete'
  form.innerHTML = `
    <label>
      <span>OAuth Callback URL</span>
      <textarea name="callback" autocomplete="off" spellcheck="false" placeholder="https://127.0.0.1:5556/?code=..."></textarea>
    </label>
    <div class="schwab-oauth-helper">只粘贴 Schwab 授权完成后的本地回调 URL。这里是 OAuth 专用入口，无需再把回调地址放进下方 Access Token 字段。</div>
    <button type="submit">完成 OAuth 连接</button>
    <div class="schwab-oauth-result" role="status" aria-live="polite"></div>
  `

  form.addEventListener('submit', async (event) => {
    event.preventDefault()
    const callback = form.elements.callback.value.trim()
    const appKey = document.querySelector('#lb-oauth-client-id')?.value.trim() || ''
    const result = form.querySelector('.schwab-oauth-result')
    const button = form.querySelector('button')

    if (!appKey) {
      result.textContent = '请先填写上方 Schwab App Key。'
      return
    }
    if (!/^https?:\/\//i.test(callback) || !/[?&](code|error)=/i.test(callback)) {
      result.textContent = '请粘贴包含 code 或 error 参数的完整 Schwab 回调 URL。'
      return
    }

    button.disabled = true
    result.textContent = '正在交换 OAuth token…'
    try {
      const response = await fetch('/api/connection', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          app_key: appKey,
          app_secret: 'server-managed',
          access_token: callback,
        }),
      })
      const payload = await response.json().catch(() => ({}))
      if (!response.ok) throw new Error(payload.detail || `HTTP ${response.status}`)
      result.textContent = 'Schwab Market Data 已连接，正在刷新工作台。'
      window.setTimeout(() => window.location.reload(), 250)
    } catch (error) {
      result.textContent = error.message || 'OAuth 连接失败。'
      button.disabled = false
    }
  })

  authorizationLink.closest('.oauth-status')?.insertAdjacentElement('afterend', form)
}

function refreshUi() {
  relabel()
  ensureOAuthCompletion()
}

const observer = new MutationObserver(refreshUi)
window.addEventListener('DOMContentLoaded', () => {
  refreshUi()
  observer.observe(document.body, { childList: true, subtree: true })
})
