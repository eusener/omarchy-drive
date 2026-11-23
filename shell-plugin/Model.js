.pragma library

// Valida e limita o status.json vindo do daemon (dado não confiável).
var MAX_PAIRS = 16
var MAX_ACCOUNTS = 16
var MAX_STR = 300
var STATES = ["idle", "syncing", "paused", "offline", "error", "needs_confirm", "auth_expired"]
var FLOW_STATES = ["authorizing", "done", "failed", "cancelled"]

function str(v) {
  return typeof v === "string" ? v.slice(0, MAX_STR) : ""
}

function num(v) {
  return typeof v === "number" && isFinite(v) && v >= 0 ? v : 0
}

function state(v) {
  return STATES.indexOf(v) >= 0 ? v : "error"
}

function parse(text) {
  var doc
  try { doc = JSON.parse(text) } catch (e) { return null }
  if (!doc || typeof doc !== "object" || typeof doc.pairs !== "object" || doc.pairs === null) return null
  var pairs = []
  var names = Object.keys(doc.pairs).slice(0, MAX_PAIRS)
  for (var i = 0; i < names.length; i++) {
    var p = doc.pairs[names[i]] || {}
    pairs.push({
      name: str(names[i]),
      provider: str(p.provider),
      local: str(p.local),
      state: state(p.state),
      lastSync: str(p.last_sync),
      lastError: str(p.last_error),
      queued: num(p.queued),
      active: Array.isArray(p.active) ? p.active.slice(0, 3).map(str) : [],
      conflicts: Array.isArray(p.conflicts) ? Math.min(p.conflicts.length, 9999) : 0,
      pendingDeletes: num(p.pending_deletes),
      quotaUsed: num(p.quota_used),
      quotaTotal: num(p.quota_total)
    })
  }
  // Contas: só existem a partir do status.json versão 2.
  var accounts = []
  if (doc.accounts && typeof doc.accounts === "object") {
    var accNames = Object.keys(doc.accounts).slice(0, MAX_ACCOUNTS)
    for (var j = 0; j < accNames.length; j++) {
      var a = doc.accounts[accNames[j]] || {}
      var accPairs = []
      for (var k = 0; k < pairs.length; k++)
        if (str(doc.pairs[pairs[k].name] && doc.pairs[pairs[k].name].account) === accNames[j]) accPairs.push(pairs[k].name)
      accounts.push({
        name: str(accNames[j]),
        provider: str(a.provider),
        label: str(a.label),
        expired: a.auth === "expired",
        pairs: accPairs
      })
    }
  }
  return { overall: state(doc.overall), pairs: pairs, accounts: accounts, accountsKnown: !!(doc.accounts && typeof doc.accounts === "object") }
}

// auth.json: progresso do fluxo de login disparado pelo painel ou pela CLI.
function parseAuth(text) {
  var doc
  try { doc = JSON.parse(text) } catch (e) { return null }
  if (!doc || typeof doc !== "object" || FLOW_STATES.indexOf(doc.state) < 0) return null
  var started = new Date(str(doc.started_at))
  return {
    state: doc.state,
    provider: str(doc.provider),
    account: str(doc.account),
    message: str(doc.message),
    pid: typeof doc.pid === "number" && doc.pid > 0 ? Math.floor(doc.pid) : 0,
    startedMs: isNaN(started.getTime()) ? 0 : started.getTime()
  }
}

// Glifos Nerd Font (Material Design).
function glyph(overall, running) {
  if (!running) return "\u{F0164}"          // cloud-off-outline
  switch (overall) {
  case "syncing": return "\u{F04E6}"        // sync
  case "paused": return "\u{F03E4}"         // pause
  case "offline": return "\u{F0164}"
  case "error":
  case "auth_expired":
  case "needs_confirm": return "\u{F0028}"  // alert-circle
  default: return "\u{F0160}"               // cloud-check
  }
}

function humanBytes(bytes, formatNumber) {
  var units = ["B", "KB", "MB", "GB", "TB"]
  var v = bytes, i = 0
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++ }
  return formatNumber(v, i === 0 ? 0 : 1) + " " + units[i]
}
