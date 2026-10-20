/**
 * `ask`: a question with a few short choices, so the person answers with one
 * key instead of typing (V44 §5.5).
 *
 * Pi's extension UI carries the question to the app: under pi-acp a `select`
 * becomes a permission request with one option per choice, which the chat
 * shows as chips and answers by picking one. Free-text questions stay in
 * prose, because the adapter can't carry an `input`.
 */

import { Type } from "@earendil-works/pi-ai";
import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";

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

export default function (pi: ExtensionAPI) {
	pi.registerTool(askTool);
}
