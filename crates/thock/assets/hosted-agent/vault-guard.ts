/**
 * The Thock Agent's vault rules, enforced where a prompt alone is not enough.
 *
 * - `append` is the tool for the most common thing the agent does: adding a
 *   section or a few lines to the end of a note. It creates the note when it
 *   is missing and never touches what is already there.
 * - `write` is refused on a note that already has content, so a whole-file
 *   rewrite can't happen by accident, and `edit`/`write` are refused outside
 *   the vault folder. A refusal comes back to the model as an error with the
 *   reason, so the next attempt can do the right thing.
 */

import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

function resolveInsideVault(cwd: string, target: string): { absolute: string; inside: boolean } {
	const absolute = resolve(cwd, target);
	const fromVault = relative(cwd, absolute);
	const inside = fromVault !== "" && !fromVault.startsWith("..") && !isAbsolute(fromVault);
	return { absolute, inside };
}

const appendTool = defineTool({
	name: "append",
	label: "Append",
	description:
		"Add text to the end of a note, after everything already in it. Creates the note when it doesn't exist. This is the tool for adding a section or new lines to a note; it never changes what is already written.",
	parameters: Type.Object({
		path: Type.String({ description: "Path of the note, relative to the vault (for example daily/2026-09-14.md)" }),
		text: Type.String({ description: "The Markdown to add at the end. Start it with a heading when adding a section." }),
	}),

	async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
		const { absolute, inside } = resolveInsideVault(ctx.cwd, params.path);
		if (!inside) {
			throw new Error(`${params.path} is outside this person's vault. Stay inside the vault folder.`);
		}
		let existing = "";
		if (existsSync(absolute)) {
			if (!statSync(absolute).isFile()) {
				throw new Error(`${params.path} is a folder, not a note.`);
			}
			existing = readFileSync(absolute, "utf-8");
		} else {
			mkdirSync(dirname(absolute), { recursive: true });
		}
		let separator = "";
		if (existing.length > 0) {
			separator = existing.endsWith("\n\n") ? "" : existing.endsWith("\n") ? "\n" : "\n\n";
		}
		const text = params.text.endsWith("\n") ? params.text : `${params.text}\n`;
		writeFileSync(absolute, existing + separator + text, "utf-8");
		const lines = text.split("\n").filter((line) => line.length > 0).length;
		return {
			content: [{ type: "text", text: `Added ${lines} line${lines === 1 ? "" : "s"} to the end of ${params.path}.` }],
			details: { path: params.path, created: existing.length === 0, lines },
		};
	},
});

export default function (pi: ExtensionAPI) {
	pi.registerTool(appendTool);

	pi.on("tool_call", (event, ctx) => {
		if (event.toolName !== "write" && event.toolName !== "edit") return undefined;
		const target = String((event.input as { path?: unknown }).path ?? "");
		if (target === "") return undefined;
		const { absolute, inside } = resolveInsideVault(ctx.cwd, target);
		if (!inside) {
			return { block: true, reason: `${target} is outside this person's vault. Stay inside the vault folder.` };
		}
		if (event.toolName === "write" && existsSync(absolute) && statSync(absolute).isFile() && statSync(absolute).size > 0) {
			return {
				block: true,
				reason: `${target} already has content, so write would erase it. Use append to add to the end of it, or edit for a small change to one spot. Never replace what the person wrote.`,
			};
		}
		return undefined;
	});
}
