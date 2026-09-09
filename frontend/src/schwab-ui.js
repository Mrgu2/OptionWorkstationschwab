const replacements = [
  ['Longbridge OpenAPI', 'Schwab Market Data API'],
  ['Longbridge OAuth 2.0', 'Schwab OAuth 2.0'],
  ['Longbridge', 'Schwab'],
  ['行情权限包', '数据权限'],
  ['Trade API', 'Trading API'],
  ['Paper Orders', 'Order API'],
  ['美股期权实时行情需要相应 OPRA 权限。', '仅使用 Schwab Market Data。账户、余额、下单和撤单接口均未启用。'],
  ['使用浏览器完成授权，不需要在工作台输入 App Secret。Access Token 只驻留 Rust 进程内存，不写入浏览器存储或本地文件。', '使用浏览器完成 Schwab 授权。当前连接表单需要 App Key、App Secret，并把最终回调地址粘贴到 Access Token 字段；凭证只驻留 Rust 进程内存。'],
  ['完成授权后此窗口会自动验证连接；不要关闭本地服务。', '授权后复制浏览器最终跳转地址，在下方凭证表单的 Access Token 字段粘贴该完整地址并验证连接。'],
]

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

const observer = new MutationObserver(() => relabel())
window.addEventListener('DOMContentLoaded', () => {
  relabel()
  observer.observe(document.body, { childList: true, subtree: true })
})
