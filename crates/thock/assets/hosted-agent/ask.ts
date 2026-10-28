/**
 * `ask`: a question with a few short choices, so the person answers with one
 * key instead of typing (V44 §5.5).
 *
 * Pi's extension UI carries the question to the app: under pi-acp a `select`
 * becomes a permission request with one option per choice, which the chat
 * shows as chips and answers by picking one. Free-text questions stay in
 * prose, because the adapter can't carry an `input`.
 *
 * `sort`: a list of items, each with its own row of choices, answered in one
 * go (V47). The chat builds the sheet from this call's own arguments, a
 * `select` titled `sort:<toolCallId>` with the one option `Apply` holds the
 * turn, and the chat writes the answers to `answers/<toolCallId>.json` in
 * Pi's config directory before it picks `Apply`, because the adapter can only
 * hand back one label.
 */

import { readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { Type } from "@earendil-works/pi-ai";
import { defineTool, getAgentDir, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

const MIN_OPTIONS = 2;
const MAX_OPTIONS = 9;

const askTool = defineTool({
	name: "ask",
	label: "Ask",
	description:
		"Ask the person to choose between two and nine short options and get back the one they picked. Use it whenever a ritual tells you to ask them to choose: yes or no, all or none or some, which of these. Ask in prose only when the answer is free text.",
	parameters: Type.Object({
		question: Type.String({ description: "One question, in a sentence." }),
		options: Type.Array(Type.String({ description: 'A short label, like "Tick it" or "Not yet".' }), {
			minItems: MIN_OPTIONS,
			maxItems: MAX_OPTIONS,
		}),
	}),

	async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
		const options = params.options.map((option) => option.trim()).filter((option) => option.length > 0);
		if (options.length < MIN_OPTIONS) {
			throw new Error(`ask needs at least ${MIN_OPTIONS} options.`);
		}
		if (options.length > MAX_OPTIONS) {
			throw new Error(`ask takes at most ${MAX_OPTIONS} options.`);
		}
		const answer = await ctx.ui.select(params.question.trim(), options);
		if (answer === undefined) {
			return {
				content: [{ type: "text", text: "They didn't answer. Carry on with the default and say so." }],
				details: { answered: false },
			};
		}
		return {
			content: [{ type: "text", text: `They chose: ${answer}` }],
			details: { answered: true, answer },
		};
	},
});

const MAX_GROUPS = 3;
const MIN_CHOICES = 2;
const MAX_CHOICES = 6;
const MAX_ITEMS = 40;
const SORT_APPLY = "Apply";

type SortGroup = {
	title: string;
	choices: string[];
	items: { id: string; text: string; note?: string; suggested?: string }[];
};

function validateSort(groups: SortGroup[]): void {
	if (groups.length < 1 || groups.length > MAX_GROUPS) {
		throw new Error(`sort takes one to ${MAX_GROUPS} groups.`);
	}
	const ids = new Set<string>();
	for (const group of groups) {
		if (group.choices.length < MIN_CHOICES || group.choices.length > MAX_CHOICES) {
			throw new Error(`Each sort group needs ${MIN_CHOICES} to ${MAX_CHOICES} choices; "${group.title}" has ${group.choices.length}.`);
		}
		if (new Set(group.choices).size !== group.choices.length) {
			throw new Error(`The choices of "${group.title}" must be different from each other.`);
		}
		for (const item of group.items) {
			if (!/^[A-Za-z0-9_-]+$/.test(item.id)) {
				throw new Error(`Item id "${item.id}" may only use letters, digits, "-" and "_".`);
			}
			if (ids.has(item.id)) {
				throw new Error(`Item id "${item.id}" is used twice; ids must be unique across groups.`);
			}
			ids.add(item.id);
			if (item.suggested !== undefined && !group.choices.includes(item.suggested)) {
				throw new Error(`"${item.suggested}" is not one of the choices of "${group.title}".`);
			}
		}
	}
	if (ids.size < 1 || ids.size > MAX_ITEMS) {
		throw new Error(`sort takes one to ${MAX_ITEMS} items in total.`);
	}
}

/**
 * The answers file's name for a call. Some providers put `|` or other
 * characters in their call ids, so anything but `[A-Za-z0-9_-]` becomes `_`;
 * the chat derives the same name (`chat_panel.rs`, `sort_answers_file`).
 */
function answersFileName(toolCallId: string): string {
	return `${toolCallId.replace(/[^A-Za-z0-9_-]/g, "_")}.json`;
}

/** Reads and removes the chat's answers, and checks every item got one of its group's choices. */
function readAnswers(path: string, groups: SortGroup[]): Record<string, string> {
	let answers: unknown;
	try {
		answers = JSON.parse(readFileSync(path, "utf8"));
	} catch {
		throw new Error("Their answers didn't come back, so nothing moved. Tell them, and ask again.");
	} finally {
		rmSync(path, { force: true });
	}
	if (typeof answers !== "object" || answers === null || Array.isArray(answers)) {
		throw new Error("The answers came back in the wrong shape.");
	}
	const result: Record<string, string> = {};
	for (const group of groups) {
		for (const item of group.items) {
			const answer = (answers as Record<string, unknown>)[item.id];
			if (typeof answer !== "string" || !group.choices.includes(answer)) {
				throw new Error(`No valid answer came back for "${item.text}".`);
			}
			result[item.id] = answer;
		}
	}
	return result;
}

const sortTool = defineTool({
	name: "sort",
	label: "Sort",
	description:
		"Ask the person to sort several items at once: each item gets one of its group's choices, all answered in one sheet. Use it instead of asking once per item. Pre-select a choice in `suggested` only when you can say why in the item's `note`; leave it out otherwise.",
	parameters: Type.Object({
		question: Type.String({ description: "One question for the whole sheet, in a sentence." }),
		groups: Type.Array(
			Type.Object({
				title: Type.String({ description: 'A short section title, like "Tasks".' }),
				choices: Type.Array(Type.String({ description: 'A short label, like "Soon".' }), {
					minItems: MIN_CHOICES,
					maxItems: MAX_CHOICES,
				}),
				items: Type.Array(
					Type.Object({
						id: Type.String({ description: 'A short id unique across groups, like "t1".' }),
						text: Type.String({ description: "The item, as the person wrote it." }),
						note: Type.Optional(
							Type.String({ description: "One short line: the evidence for the suggestion, or useful context." }),
						),
						suggested: Type.Optional(Type.String({ description: "One of this group's choices." })),
					}),
					{ minItems: 1 },
				),
			}),
			{ minItems: 1, maxItems: MAX_GROUPS },
		),
		footnote: Type.Optional(Type.String({ description: "One line on what you left out, like items already handled." })),
	}),

	async execute(toolCallId, params, _signal, _onUpdate, ctx) {
		validateSort(params.groups);
		const answersPath = join(getAgentDir(), "answers", answersFileName(toolCallId));
		const applied = await ctx.ui.select(`sort:${toolCallId}`, [SORT_APPLY]);
		if (applied === undefined) {
			rmSync(answersPath, { force: true });
			return {
				content: [{ type: "text", text: "They didn't answer. Nothing moves." }],
				details: { answered: false },
			};
		}
		const answers = readAnswers(answersPath, params.groups);
		const summary = params.groups
			.flatMap((group) => group.items.map((item) => `${item.id} → ${answers[item.id]}`))
			.join(", ");
		return {
			content: [{ type: "text", text: `They sorted: ${summary}` }],
			details: { answered: true, answers },
		};
	},
});

export default function (pi: ExtensionAPI) {
	pi.registerTool(askTool);
	pi.registerTool(sortTool);
}
