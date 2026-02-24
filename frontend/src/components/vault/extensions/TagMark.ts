import { Mark, mergeAttributes } from "@tiptap/core";
import { InputRule } from "@tiptap/core";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";

export interface TagMarkOptions {
  onTagClick: (tag: string) => void;
  HTMLAttributes: Record<string, unknown>;
}

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    tagMark: {
      setTag: (tagName: string) => ReturnType;
    };
  }
}

export const TagMark = Mark.create<TagMarkOptions>({
  name: "tagMark",

  addOptions() {
    return {
      onTagClick: () => {},
      HTMLAttributes: {},
    };
  },

  parseHTML() {
    return [
      {
        tag: 'span[data-tag]',
      },
    ];
  },

  renderHTML({ HTMLAttributes }) {
    return [
      "span",
      mergeAttributes(this.options.HTMLAttributes, HTMLAttributes, {
        "data-tag": "",
        class: "tag-mark",
        style: [
          "background: #5C4033",
          "color: #DCD5CC",
          "padding: 1px 8px",
          "border-radius: 9999px",
          "font-size: 12px",
          "cursor: pointer",
          "display: inline-block",
          "line-height: 1.4",
        ].join("; "),
      }),
      0,
    ];
  },

  addAttributes() {
    return {
      tagName: {
        default: null,
        parseHTML: (element) => element.getAttribute("data-tag-name"),
        renderHTML: (attributes) => ({
          "data-tag-name": attributes.tagName as string,
        }),
      },
    };
  },

  addCommands() {
    return {
      setTag:
        (tagName: string) =>
        ({ commands }) => {
          return commands.setMark(this.name, { tagName });
        },
    };
  },

  addInputRules() {
    return [
      new InputRule({
        find: /(?:^|\s)#([\w-]+)\s$/,
        handler: ({ state, range, match }) => {
          const tagName = match[1];
          const { tr } = state;
          // Calculate the position of the #tag part (skip leading whitespace)
          const hasLeadingSpace = match[0].startsWith(" ");
          const tagStart = range.from + (hasLeadingSpace ? 1 : 0);
          const tagText = `#${tagName}`;

          tr.replaceWith(tagStart, range.to, state.schema.text(tagText + " "));
          tr.addMark(
            tagStart,
            tagStart + tagText.length,
            this.type.create({ tagName })
          );

          return;
        },
      }),
    ];
  },

  addProseMirrorPlugins() {
    const onTagClick = this.options.onTagClick;
    return [
      new Plugin({
        key: new PluginKey("tagMarkClick"),
        props: {
          handleClick(_view: EditorView, _pos: number, event: MouseEvent) {
            const target = event.target as HTMLElement;
            if (target.hasAttribute("data-tag")) {
              const tagName = target.getAttribute("data-tag-name");
              if (tagName) {
                onTagClick(tagName);
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
