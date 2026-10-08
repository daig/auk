-- tern.ui: the view builders (research/plugins.md §8.2). Builders return plain tables in the
-- TSP wire form {k, p?, c?}; p and c are left out when empty. Bad arguments raise
-- `tern.ui.<builder>: <arg path>: <problem>`.
local native = ...

local ui = {}

local function fail(builder: string, path: string, problem: string): never
	error(string.format("tern.ui.%s: %s: %s", builder, path, problem), 0)
end

local function kind_of(v: any): string
	return typeof(v)
end

local function str(b: string, path: string, v: any): string
	local t = type(v)
	if t == "string" then
		return v
	elseif t == "number" then
		return tostring(v)
	end
	fail(b, path, "expected a string, got " .. kind_of(v))
	return ""
end

local function optstr(b: string, path: string, v: any): string?
	if v == nil then
		return nil
	end
	return str(b, path, v)
end

local function num(b: string, path: string, v: any): number
	if type(v) ~= "number" then
		fail(b, path, "expected a number, got " .. kind_of(v))
	end
	return v
end

local function int(b: string, path: string, v: any, lo: number, hi: number): number
	if type(v) ~= "number" or v ~= math.floor(v) or v < lo or v > hi then
		if type(v) ~= "number" then
			fail(b, path, "expected a number, got " .. kind_of(v))
		end
		fail(b, path, string.format("expected an integer in %d..=%d, got %s", lo, hi, tostring(v)))
	end
	return v
end

local function list(b: string, path: string, v: any): { any }
	if v == nil then
		return {}
	end
	if type(v) ~= "table" then
		fail(b, path, "expected a list, got " .. kind_of(v))
	end
	return v
end

local function tbl(b: string, path: string, v: any): { [any]: any }
	if type(v) ~= "table" then
		fail(b, path, "expected a table, got " .. kind_of(v))
	end
	return v
end

local function is_span(v: any): boolean
	return type(v) == "table" and type(v.t) == "string"
end

local function span_of(b: string, path: string, v: any): { [string]: any }
	if type(v) == "string" or type(v) == "number" then
		return { t = tostring(v) }
	end
	if type(v) == "table" then
		if type(v.t) ~= "string" then
			fail(b, path, "expected a span (a table with a string `t`)")
		end
		local s = { t = v.t }
		if v.s ~= nil and v.s ~= "" then
			s.s = str(b, path .. ".s", v.s)
		end
		if v.href ~= nil then
			s.href = str(b, path .. ".href", v.href)
		end
		if v.fx ~= nil then
			s.fx = str(b, path .. ".fx", v.fx)
		end
		return s
	end
	fail(b, path, "expected a span or a string, got " .. kind_of(v))
	return {}
end

-- A span list from `Spans`: a list of spans and strings, one span, or one string; nil is empty.
local function spans(b: string, path: string, v: any): { any }
	if v == nil then
		return {}
	end
	if type(v) == "string" or type(v) == "number" or is_span(v) then
		return { span_of(b, path, v) }
	end
	if type(v) ~= "table" then
		fail(b, path, "expected a span or a string, got " .. kind_of(v))
	end
	local out = {}
	for i, item in ipairs(v) do
		table.insert(out, span_of(b, string.format("%s[%d]", path, i), item))
	end
	return out
end

local function is_node(v: any): boolean
	return type(v) == "table" and type(v.k) == "string"
end

local function nodes(b: string, path: string, v: any): { any }
	if v == nil then
		return {}
	end
	if type(v) ~= "table" then
		fail(b, path, "expected a list, got " .. kind_of(v))
	end
	if is_node(v) then
		fail(b, path, "expected a list of nodes, got one node (wrap it in {})")
	end
	for i, n in ipairs(v) do
		if not is_node(n) then
			fail(b, string.format("%s[%d]", path, i), "expected a node (a table with a string `k`)")
		end
	end
	return v
end

local function node(k: string, p: { [string]: any }?, c: { any }?): { [string]: any }
	local n: { [string]: any } = { k = k }
	if p and next(p) ~= nil then
		n.p = p
	end
	if c and #c > 0 then
		n.c = c
	end
	return n
end

local function clamp01(v: number): number
	if v ~= v then
		return 0
	end
	return math.clamp(v, 0, 1)
end

-- Percent-encodes a path for a file:// URL.
local function file_url(path: string): string
	return "file://" .. string.gsub(path, "[^%w%-%._~/]", function(c: string): string
		return string.format("%%%02X", string.byte(c))
	end)
end

local function join(dir: string, p: string): string
	if string.sub(dir, -1) == "/" then
		return dir .. p
	end
	return dir .. "/" .. p
end

function ui.span(t: any, s: any)
	local out: { [string]: any } = { t = str("span", "t", t) }
	local style = optstr("span", "s", s)
	if style and style ~= "" then
		out.s = style
	end
	return out
end

function ui.link(t: any, href: any, s: any)
	local out: { [string]: any } = { t = str("link", "t", t), href = str("link", "href", href) }
	local style = optstr("link", "s", s)
	if style and style ~= "" then
		out.s = style
	end
	return out
end

function ui.path(p: any, cwd: any)
	local text = str("path", "p", p)
	local base = optstr("path", "cwd", cwd)
	local out: { [string]: any } = { t = text, s = "path" }
	if string.sub(text, 1, 1) == "~" then
		return out
	end
	if string.sub(text, 1, 1) == "/" then
		out.href = file_url(text)
	elseif base and string.sub(base, 1, 1) == "/" then
		out.href = file_url(join(base, (string.gsub(text, "^%./", ""))))
	end
	return out
end

function ui.text(s: any)
	return node("text", { spans = spans("text", "spans", s) })
end

function ui.col(children: any)
	return node("col", { gap = "sm" }, nodes("col", "children", children))
end

function ui.lines(children: any)
	return node("col", { gap = "none" }, nodes("lines", "children", children))
end

function ui.row(children: any)
	return node("row", { gap = "sm" }, nodes("row", "children", children))
end

function ui.card(head: any, children: any)
	return node("card", { head = spans("card", "head", head) }, nodes("card", "children", children))
end

function ui.section(head: any, children: any)
	return node("section", { head = spans("section", "head", head) }, nodes("section", "children", children))
end

function ui.rule(label: any)
	local l = spans("rule", "label", label)
	local empty = #l == 0 or (#l == 1 and l[1].t == "")
	return node("rule", if empty then nil else { label = l })
end

function ui.badge(t: any, tone: any)
	return node("badge", { text = str("badge", "t", t), tone = optstr("badge", "tone", tone) })
end

function ui.kv(items: any)
	local out = {}
	for i, pair in ipairs(list("kv", "items", items)) do
		local path = string.format("items[%d]", i)
		if type(pair) ~= "table" or pair[1] == nil and pair[2] == nil then
			fail("kv", path, "expected a {a, b} pair, got " .. kind_of(pair))
		end
		table.insert(out, { k = spans("kv", path .. "[1]", pair[1]), v = spans("kv", path .. "[2]", pair[2]) })
	end
	return node("kv", { items = out })
end

function ui.meter(value: any, label: any)
	return node("meter", { value = clamp01(num("meter", "value", value)), label = spans("meter", "label", label) })
end

function ui.progress(value: any, label: any)
	local v = if value == nil then nil else clamp01(num("progress", "value", value))
	return node("progress", { value = v, label = spans("progress", "label", label) })
end

local function series(b: string, s: any): { any }
	local out = {}
	for i, pair in ipairs(list(b, "series", s)) do
		local path = string.format("series[%d]", i)
		if type(pair) ~= "table" then
			fail(b, path, "expected a {a, b} pair, got " .. kind_of(pair))
		end
		table.insert(out, { label = str(b, path .. "[1]", pair[1]), value = num(b, path .. "[2]", pair[2]) })
	end
	return out
end

function ui.bars(s: any)
	return node("chart", { kind = "bars", series = series("bars", s) })
end

function ui.spark(s: any)
	return node("chart", { kind = "spark", series = series("spark", s) })
end

local ALIGN = { start = true, center = true, ["end"] = true }
local TRUNCATE = { ["end"] = true, start = true, middle = true }

local function cell(b: string, path: string, v: any): any
	if type(v) == "table" and v.meter ~= nil then
		local m = tbl(b, path .. ".meter", v.meter)
		local out: { [string]: any } = {}
		if m.value ~= nil then
			out.value = clamp01(num(b, path .. ".meter.value", m.value))
		end
		if m.thresholds ~= nil then
			local th = tbl(b, path .. ".meter.thresholds", m.thresholds)
			out.thresholds = {
				warn = num(b, path .. ".meter.thresholds.warn", th.warn),
				bad = num(b, path .. ".meter.thresholds.bad", th.bad),
			}
		end
		if m.parts ~= nil then
			local parts = {}
			for i, part in ipairs(list(b, path .. ".meter.parts", m.parts)) do
				local pp = string.format("%s.meter.parts[%d]", path, i)
				local t = tbl(b, pp, part)
				local o: { [string]: any } = { value = num(b, pp .. ".value", t.value), token = str(b, pp .. ".token", t.token) }
				if t.label ~= nil then
					o.label = str(b, pp .. ".label", t.label)
				end
				if t.hatch ~= nil then
					o.hatch = t.hatch == true
				end
				table.insert(parts, o)
			end
			out.parts = parts
		end
		if m.tone ~= nil then
			out.tone = str(b, path .. ".meter.tone", m.tone)
		end
		if m.title ~= nil then
			out.title = str(b, path .. ".meter.title", m.title)
		end
		return { meter = out }
	end
	return spans(b, path, v)
end

function ui.table(cols: any, rows: any)
	local outcols = {}
	local ids = {}
	for i, c in ipairs(list("table", "cols", cols)) do
		local path = string.format("cols[%d]", i)
		local col = tbl("table", path, c)
		local o: { [string]: any } = {
			id = str("table", path .. ".id", col.id),
			head = str("table", path .. ".head", col.head),
			align = "start",
			truncate = "end",
			priority = 0,
			grow = 0,
		}
		if col.align ~= nil then
			local a = str("table", path .. ".align", col.align)
			if not ALIGN[a] then
				fail("table", path .. ".align", string.format('unknown align "%s" (start|center|end)', a))
			end
			o.align = a
		end
		if col.truncate ~= nil then
			local t = str("table", path .. ".truncate", col.truncate)
			if not TRUNCATE[t] then
				fail("table", path .. ".truncate", string.format('unknown truncate "%s" (end|start|middle)', t))
			end
			o.truncate = t
		end
		if col.priority ~= nil then
			o.priority = int("table", path .. ".priority", col.priority, -1000000, 1000000)
		end
		if col.grow ~= nil then
			o.grow = num("table", path .. ".grow", col.grow)
		end
		-- A later column with the same id wins.
		if ids[o.id] then
			outcols[ids[o.id]] = o
		else
			table.insert(outcols, o)
			ids[o.id] = #outcols
		end
		ids[i] = o.id
	end
	local outrows = {}
	for r, row in ipairs(list("table", "rows", rows)) do
		local path = string.format("rows[%d]", r)
		local cells = {}
		for j, v in ipairs(list("table", path, row)) do
			local id = ids[j]
			if id ~= nil then
				cells[id] = cell("table", string.format("%s[%d]", path, j), v)
			end
		end
		table.insert(outrows, { id = "r" .. (r - 1), cells = cells })
	end
	return node("table", { cols = outcols, rows = outrows })
end

function ui.meter_cell(value: any, thresholds: any)
	local m: { [string]: any } = { value = clamp01(num("meter_cell", "value", value)) }
	if thresholds ~= nil then
		local th = tbl("meter_cell", "thresholds", thresholds)
		m.thresholds = { warn = num("meter_cell", "thresholds[1]", th[1]), bad = num("meter_cell", "thresholds[2]", th[2]) }
	end
	return { meter = m }
end

function ui.meter_parts_cell(parts: any)
	local out = {}
	for i, part in ipairs(list("meter_parts_cell", "parts", parts)) do
		local path = string.format("parts[%d]", i)
		if type(part) ~= "table" then
			fail("meter_parts_cell", path, "expected a {a, b} pair, got " .. kind_of(part))
		end
		table.insert(out, { value = num("meter_parts_cell", path .. "[1]", part[1]), token = str("meter_parts_cell", path .. "[2]", part[2]) })
	end
	return { meter = { parts = out } }
end

function ui.tree(input: any)
	local counter = 0
	local function walk(items: any, path: string): { any }
		local out = {}
		for i, item in ipairs(list("tree", path, items)) do
			local p = string.format("%s[%d]", path, i)
			local t = tbl("tree", p, item)
			local o: { [string]: any } = { id = "t" .. counter, label = spans("tree", p .. ".label", t.label) }
			counter += 1
			if t.icon ~= nil then
				o.icon = str("tree", p .. ".icon", t.icon)
			end
			if t.children ~= nil and #list("tree", p .. ".children", t.children) > 0 then
				o.open = t.open == true
				o.children = walk(t.children, p .. ".children")
			end
			table.insert(out, o)
		end
		return out
	end
	return node("tree", { nodes = walk(input, "nodes") })
end

function ui.list(items: any)
	local out = {}
	for i, item in ipairs(list("list", "items", items)) do
		local path = string.format("items[%d]", i)
		local t = tbl("list", path, item)
		local p: { [string]: any } = { label = spans("list", path .. ".label", t.label) }
		if t.detail ~= nil then
			p.detail = spans("list", path .. ".detail", t.detail)
		end
		if t.value ~= nil then
			p.value = spans("list", path .. ".value", t.value)
		end
		if t.icon ~= nil then
			p.icon = str("list", path .. ".icon", t.icon)
		end
		if t.tone ~= nil then
			p.tone = str("list", path .. ".tone", t.tone)
		end
		table.insert(out, node("item", p))
	end
	return node("list", nil, out)
end

function ui.code(text: any, lang: any, start: any)
	local p: { [string]: any } = { text = str("code", "text", text), lang = optstr("code", "lang", lang) }
	if start ~= nil then
		p.start = int("code", "start", start, 0, 2 ^ 53)
		p.numbers = true
	end
	return node("code", p)
end

function ui.diff(text: any, path: any)
	return node("diff", { text = str("diff", "text", text), path = optstr("diff", "path", path) })
end

function ui.md(text: any)
	return node("md", { text = str("md", "text", text) })
end

function ui.ansi(text: any)
	return node("ansi", { text = str("ansi", "text", text) })
end

local SEVERITY = {
	error = { tone = "error", label = "error" },
	warning = { tone = "warning", label = "warning" },
	note = { tone = "info", label = "note" },
	help = { tone = "muted", label = "help" },
}

function ui.diagnostic(d: any, cwd: any)
	local t = tbl("diagnostic", "d", d)
	local base = optstr("diagnostic", "cwd", cwd)
	local severity = optstr("diagnostic", "d.severity", t.severity) or "error"
	local sev = SEVERITY[severity]
	if sev == nil then
		fail("diagnostic", "d.severity", string.format('unknown severity "%s" (error|warning|note|help)', severity))
	end
	local message = str("diagnostic", "d.message", t.message)
	local code = optstr("diagnostic", "d.code", t.code)
	local path = optstr("diagnostic", "d.path", t.path)
	local line = if t.line ~= nil then num("diagnostic", "d.line", t.line) else nil
	local col = if t.col ~= nil then num("diagnostic", "d.col", t.col) else nil
	local head: { any } = { { t = sev.label .. (if code then "[" .. code .. "]" else ""), s = "strong " .. sev.tone }, { t = " " .. message } }
	local children: { any } = {}
	if path then
		local loc = path
		if line then
			loc ..= ":" .. line
			if col then
				loc ..= ":" .. col
			end
		end
		local link = ui.path(path, base)
		link.t = loc
		if link.href and line then
			link.href ..= "#L" .. line .. (if col then "C" .. col else "")
		end
		table.insert(children, node("text", { spans = { link } }))
	end
	if t.snippet ~= nil then
		local sn = tbl("diagnostic", "d.snippet", t.snippet)
		local first = num("diagnostic", "d.snippet[1]", sn[1])
		local source = str("diagnostic", "d.snippet[2]", sn[2])
		table.insert(children, node("code", { text = source, start = first, numbers = true }))
	end
	for i, n in ipairs(list("diagnostic", "d.notes", t.notes)) do
		table.insert(children, node("text", { spans = { { t = str("diagnostic", string.format("d.notes[%d]", i), n), s = "muted" } } }))
	end
	for i, s in ipairs(list("diagnostic", "d.suggestions", t.suggestions)) do
		local p = string.format("d.suggestions[%d]", i)
		local sg = tbl("diagnostic", p, s)
		table.insert(children, node("text", { spans = { { t = str("diagnostic", p .. "[1]", sg[1]), s = "info" } } }))
		if sg[3] ~= nil then
			table.insert(children, node("code", { text = str("diagnostic", p .. "[3]", sg[3]), start = sg[2], numbers = sg[2] ~= nil }))
		end
	end
	local key = (path or "") .. ":" .. tostring(line or "") .. ":" .. tostring(col or "") .. ":" .. (code or "")
	return node("card", { head = head, tone = sev.tone, key = key }, children)
end

function ui.test_summary(passed: any, failed: any, skipped: any, took: any)
	local p = num("test_summary", "passed", passed)
	local f = num("test_summary", "failed", failed)
	local s = num("test_summary", "skipped", skipped)
	local tk = optstr("test_summary", "took", took)
	local total = p + f + s
	local parts = {}
	local words = {}
	if p > 0 or total == 0 then
		table.insert(words, p .. " passed")
	end
	if f > 0 then
		table.insert(words, f .. " failed")
	end
	if s > 0 then
		table.insert(words, s .. " skipped")
	end
	if tk then
		table.insert(words, tk)
	end
	if total > 0 then
		table.insert(parts, { value = p / total, token = "success" })
		table.insert(parts, { value = f / total, token = "error" })
		table.insert(parts, { value = s / total, token = "muted" })
	end
	return node("meter", {
		value = if total > 0 then p / total else 0,
		parts = parts,
		label = { { t = table.concat(words, " · ") } },
		tone = if f > 0 then "error" else "success",
	})
end

function ui.overflow(hidden: any)
	local n = num("overflow", "hidden", hidden)
	return node("text", { spans = { { t = string.format("… %d more (Raw shows everything)", n), s = "muted" } } })
end

function ui.node(kind: any, props: any, children: any)
	local k = str("node", "kind", kind)
	local p = nil
	if props ~= nil then
		p = tbl("node", "props", props)
	end
	return node(k, p, nodes("node", "children", children))
end

function ui.el(tag: any, props: any, children: any)
	local p: { [string]: any } = { tag = str("el", "tag", tag) }
	if props ~= nil then
		for k, v in tbl("el", "props", props) do
			if k ~= "tag" then
				p[k] = v
			end
		end
	end
	return node("el", p, nodes("el", "children", children))
end

-- tern.parse (research/plugins.md §8.8). Unlike tern.ui, strings are required.
local parse = {}

local function pfail(f: string, path: string, problem: string): never
	error(string.format("tern.parse.%s: %s: %s", f, path, problem), 0)
end

local function strings(f: string, v: any): { string }
	if type(v) ~= "table" then
		pfail(f, "lines", "expected a list of strings, got " .. kind_of(v))
	end
	for i, s in ipairs(v) do
		if type(s) ~= "string" then
			pfail(f, string.format("lines[%d]", i), "expected a string, got " .. kind_of(s))
		end
	end
	return v
end

local function chars(s: string): { string }
	local out = {}
	for _, c in utf8.codes(s) do
		table.insert(out, utf8.char(c))
	end
	return out
end

-- Splits lines at display columns that are blank in every line; returns each line's cells.
local function split_columns(lines: { string }): ({ { string } }, number)
	local rows = {}
	local width = 0
	for _, line in lines do
		local cs = if utf8.len(line) then chars(line) else string.split(line, "")
		table.insert(rows, cs)
		width = math.max(width, #cs)
	end
	local blank = table.create(width, true)
	for _, cs in rows do
		for i, c in cs do
			if c ~= " " and c ~= "\t" then
				blank[i] = false
			end
		end
	end
	local ranges = {}
	local i = 1
	while i <= width do
		if not blank[i] then
			local j = i
			while j + 1 <= width and not blank[j + 1] do
				j += 1
			end
			table.insert(ranges, { i, j })
			i = j + 1
		else
			i += 1
		end
	end
	local out = {}
	for _, cs in rows do
		local cells = {}
		for _, r in ranges do
			local text = table.concat(cs, "", math.min(r[1], #cs + 1), math.min(r[2], #cs))
			table.insert(cells, (string.gsub(text, "^%s*(.-)%s*$", "%1")))
		end
		table.insert(out, cells)
	end
	return out, #ranges
end

function parse.columns(input: any)
	local lines = {}
	for _, l in strings("columns", input) do
		if string.find(l, "%S") then
			table.insert(lines, l)
		end
	end
	if #lines == 0 then
		return nil
	end
	-- Prose: no two header words two blanks apart.
	if not string.find(lines[1], "%S  +%S") then
		return nil
	end
	local cells, n = split_columns(lines)
	if n < 2 then
		return nil
	end
	local rows = {}
	for i = 2, #cells do
		table.insert(rows, cells[i])
	end
	return { head = cells[1], rows = rows }
end

function parse.packed(input: any)
	local lines = {}
	for _, l in strings("packed", input) do
		if string.find(l, "%S") then
			table.insert(lines, l)
		end
	end
	if #lines == 0 then
		return {}
	end
	local cells, n = split_columns(lines)
	local out = {}
	for c = 1, n do
		for _, row in cells do
			local v = row[c]
			if v and v ~= "" then
				table.insert(out, v)
			end
		end
	end
	return out
end

function parse.json_tree(text: any)
	if type(text) ~= "string" then
		pfail("json_tree", "text", "expected a string, got " .. kind_of(text))
	end
	return native.json_tree(text)
end

function parse.location(s: any)
	if type(s) ~= "string" then
		pfail("location", "s", "expected a string, got " .. kind_of(s))
	end
	local path, line, col = string.match(s, "^([^:%s][^:]*):(%d+):(%d+)")
	if path then
		return path, tonumber(line), tonumber(col)
	end
	path, line = string.match(s, "^([^:%s][^:]*):(%d+)")
	if path then
		return path, tonumber(line), nil
	end
	return nil, nil, nil
end

local UNITS = { B = 0, K = 1, M = 2, G = 3, T = 4, P = 5, E = 6 }

function parse.size(s: any)
	if type(s) ~= "string" then
		pfail("size", "s", "expected a string, got " .. kind_of(s))
	end
	local text = string.gsub(s, "^%s*(.-)%s*$", "%1")
	if text == "" or text == "-" then
		return nil
	end
	local n, unit = string.match(text, "^([%d%.]+)%s*(%a*)$")
	local value = tonumber(n)
	if value == nil then
		return nil
	end
	if unit == "" then
		return value
	end
	local u = string.upper(unit)
	local letter = string.sub(u, 1, 1)
	local power = UNITS[letter]
	if power == nil then
		return nil
	end
	local rest = string.sub(u, 2)
	if letter == "B" then
		return if rest == "" then value else nil
	end
	if rest == "" or rest == "I" or rest == "IB" then
		return value * 1024 ^ power
	elseif rest == "B" then
		return value * 1000 ^ power
	end
	return nil
end

return ui, parse
