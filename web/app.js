"use strict";
const $ = (id) => document.getElementById(id);
const state = {
	stats: null,
	file: null,
	lines: [],
	mode: "all",
	generation: 0,
};
const number = (n) => new Intl.NumberFormat().format(n);
const percent = (n) => `${n.toFixed(1)}%`;
function element(tag, className, text) {
	const node = document.createElement(tag);
	if (className) node.className = className;
	if (text !== undefined) node.textContent = text;
	return node;
}
async function api(url) {
	const response = await fetch(url, { cache: "no-store" });
	const data = await response.json();
	if (!response.ok) throw new Error(data.error || "Could not read the ledger");
	return data;
}
function status(message, error = false) {
	$("status").textContent = message;
	$("status").classList.toggle("error", error);
}
function visibleFiles() {
	const query = $("file-search").value.toLowerCase();
	const filter = $("file-filter").value;
	const files = state.stats.files.filter(
		(f) =>
			f.file.toLowerCase().includes(query) &&
			(filter === "all" ||
				(filter === "ai" && f.ai > 0) ||
				(filter === "human" && f.ai === 0) ||
				(filter === "pending" && f.pending > 0)),
	);
	const sort = $("sort").value;
	files.sort((a, b) =>
		sort === "name"
			? a.file.localeCompare(b.file)
			: (sort === "size" ? b.total - a.total : b.pct - a.pct) ||
				a.file.localeCompare(b.file),
	);
	return files;
}
function renderFiles() {
	if (!state.stats) return;
	const list = $("file-list");
	list.replaceChildren();
	const files = visibleFiles();
	$("shown-count").textContent =
		`${files.length} / ${state.stats.files.length}`;
	if (!files.length) {
		list.append(
			element(
				"p",
				"file-empty",
				state.stats.files.length
					? "No files match your filters."
					: "No text files yet. Add a source file, then refresh.",
			),
		);
		return;
	}
	for (const file of files) {
		const button = element(
			"button",
			`file-row${file.ai ? " has-ai" : ""}${state.file === file.file ? " active" : ""}`,
		);
		button.setAttribute(
			"aria-current",
			state.file === file.file ? "true" : "false",
		);
		button.title = `${file.file} · ${file.ai} AI / ${file.total} lines`;
		const extension = file.file.split(".").pop();
		button.append(
			element("span", "file-ext", extension.length <= 4 ? extension : "{}"),
			element("span", "file-name", file.file),
			element("span", "file-pct", `${Math.round(file.pct)}%`),
		);
		const bar = document.createElement("progress");
		bar.className = "mini-bar";
		bar.max = 100;
		bar.value = file.pct;
		bar.setAttribute("aria-label", `${file.file} AI contribution`);
		button.append(bar);
		button.addEventListener("click", () => selectFile(file.file));
		list.append(button);
	}
}
function renderLines() {
	const source = $("source");
	source.replaceChildren();
	const query = $("line-search").value.toLowerCase();
	const lines = state.lines.filter(
		(l) =>
			(state.mode === "all" ||
				(state.mode === "ai" && l.ai) ||
				(state.mode === "human" && !l.ai) ||
				(state.mode === "pending" && l.pending)) &&
			(!query ||
				l.text.toLowerCase().includes(query) ||
				l.author.toLowerCase().includes(query)),
	);
	const fragment = document.createDocumentFragment();
	for (const line of lines) {
		const row = element(
			"div",
			`code-line ${line.ai ? "ai" : "human"}${line.pending ? " pending" : ""}`,
		);
		const who = line.pending ? "human · pending" : line.author;
		const author = element("span", "line-author");
		author.title = `${who}${line.commit ? ` · ${line.commit.slice(0, 12)}` : " · Unrecorded change"}`;
		author.append(
			element(
				"i",
				line.pending ? "pending-dot" : line.ai ? "ai-dot" : "human-dot",
			),
			document.createTextNode(who),
		);
		row.append(
			element("span", "line-number", line.line),
			author,
			element("pre", "line-text", line.text || " "),
		);
		fragment.append(row);
	}
	if (!lines.length) {
		const empty = element("div", "empty");
		empty.append(
			element(
				"h3",
				"",
				state.lines.length ? "No matching lines." : "This file is empty.",
			),
		);
		fragment.append(empty);
	}
	source.append(fragment);
	$("line-count").textContent =
		`${number(lines.length)} of ${number(state.lines.length)} lines`;
}
async function selectFile(file) {
	const generation = ++state.generation;
	state.file = file;
	state.lines = [];
	renderFiles();
	$("file-title").textContent = file;
	$("file-summary").textContent = "";
	$("source").replaceChildren(element("div", "empty", "Reading attribution…"));
	try {
		const lines = await api(`/api/blame?file=${encodeURIComponent(file)}`);
		if (generation !== state.generation) return;
		state.lines = lines;
		const ai = lines.filter((l) => l.ai).length;
		$("file-summary").textContent =
			`${number(lines.length)} lines · ${number(ai)} AI`;
		renderLines();
	} catch (error) {
		if (generation === state.generation) {
			$("source").replaceChildren(element("div", "empty", error.message));
			$("line-count").textContent = "File unavailable";
			status(error.message, true);
		}
	}
}
async function refresh() {
	$("refresh").disabled = true;
	status("Reading your local ledger…");
	try {
		state.stats = await api("/api/stat");
		const s = state.stats;
		$("pct").textContent = percent(s.pct);
		$("human").textContent = number(s.human);
		$("ai").textContent = number(s.ai);
		$("files-count").textContent = number(s.files.length);
		$("contribution").value = s.pct;
		$("scope").textContent = s.skipped
			? `${s.skipped} non-text or large files skipped`
			: "In this project";
		status(
			s.pending
				? `${number(s.pending)} pending lines are unrecorded and currently count as human. Refresh after an agent finishes.`
				: "Ledger is up to date. Refresh to pick up new edits.",
		);
		renderFiles();
		const file = s.files.some((f) => f.file === state.file)
			? state.file
			: s.files[0]?.file;
		if (file) await selectFile(file);
		else {
			state.file = null;
			state.lines = [];
			state.generation++;
			$("file-title").textContent = "No files yet";
			$("file-summary").textContent = "";
			$("source").replaceChildren(
				element(
					"div",
					"empty",
					"Add a text file to this project, then refresh.",
				),
			);
			$("line-count").textContent = "No file selected";
		}
	} catch (error) {
		status(error.message, true);
	} finally {
		$("refresh").disabled = false;
	}
}
$("refresh").addEventListener("click", refresh);
$("file-search").addEventListener("input", renderFiles);
$("file-filter").addEventListener("change", renderFiles);
$("sort").addEventListener("change", renderFiles);
$("line-search").addEventListener("input", () => {
	if (state.file) renderLines();
});
document.querySelectorAll("[data-lines]").forEach((button) =>
	button.addEventListener("click", () => {
		state.mode = button.dataset.lines;
		document
			.querySelectorAll("[data-lines]")
			.forEach((b) => b.setAttribute("aria-pressed", String(b === button)));
		if (state.file) renderLines();
	}),
);
document.addEventListener("keydown", (event) => {
	if (
		event.key === "/" &&
		!["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement.tagName)
	) {
		event.preventDefault();
		$("file-search").focus();
	}
});
refresh();
