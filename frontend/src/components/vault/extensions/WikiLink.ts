import { Mark, mergeAttributes } from "@tiptap/core";
import { InputRule } from "@tiptap/core";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";

export interface WikiLinkOptions {
  onNavigate: (noteName: string) => void;
  onSuggestion: (query: string) => string[];
  HTMLAttributes: Record<string, unknown>;
}

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    wikiLink: {
      setWikiLink: (noteName: string) => ReturnType;
    };
  }
}

export const WikiLink = Mark.create<WikiLinkOptions>({
  name: "wikiLink",

  addOptions() {
    return {
      onNavigate: () => {},
      onSuggestion: () => [],
      HTMLAttributes: {},
    };
  },

  parseHTML() {
    return [
      {
        tag: 'span[data-wiki-link]',
      },
    ];
  },

  renderHTML({ HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(this.options.HTMLAttributes, HTMLAttributes, {
        "data-wiki-link": "",
        class: "wiki-link",
        style: "color: #5C4033; cursor: pointer; text-decoration: underline; text-decoration-color: transparent; transition: text-decoration-color 0.15s;",
      }),
      0,
    ];
  },

  addAttributes() {
    return {
      noteName: {
        default: null,
        parseHTML: (element) => element.getAttribute("data-note-name"),
        renderHTML: (attributes) => ({
          "data-note-name": attributes.noteName as string,
        }),
      },
    };
  },

  addCommands() {
    return {
      setWikiLink:
        (noteName: string) =>
        ({ commands }) => {
          return commands.setMark(this.name, { noteName });
        },
    };
  },

  addInputRules() {
    return [
      new InputRule({
        find: /\[\[([^\]]+)\]\]$/,
        handler: ({ state, range, match }) => {
          const noteName = match[1];
          const { tr } = state;
          const start = range.from;
          const end = range.to;

          tr.replaceWith(start, end, state.schema.text(`[[${noteName}]]`));
          tr.addMark(
            start,
            start + noteName.length + 4,
            this.type.create({ noteName })
          );

          return;
        },
      }),
    ];
  },

  addProseMirrorPlugins() {
    const onNavigate = this.options.onNavigate;
    return [
      new Plugin({
        key: new PluginKey("wikiLinkClick"),
        props: {
          handleClick(_view: EditorView, _pos: number, event: MouseEvent) {
            const target = event.target as HTMLElement;
            if (target.hasAttribute("data-wiki-link")) {
              const noteName = target.getAttribute("data-note-name");
              if (noteName) {
                onNavigate(noteName);
                return true;
              }
            }
            return false;
          },
        },
      }),
    ];
  },
});
