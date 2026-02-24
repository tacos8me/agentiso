import { Node, mergeAttributes } from "@tiptap/core";
import { Plugin, PluginKey } from "@tiptap/pm/state";
import type { Node as ProsemirrorNode } from "@tiptap/pm/model";

export interface FrontmatterBlockOptions {
  HTMLAttributes: Record<string, unknown>;
}

declare module "@tiptap/core" {
  interface Commands<ReturnType> {
    frontmatterBlock: {
      setFrontmatter: (content: string) => ReturnType;
    };
  }
}

export const FrontmatterBlock = Node.create<FrontmatterBlockOptions>({
  name: "frontmatterBlock",
  group: "block",
  content: "text*",
  marks: "",
  code: true,
  defining: true,
  isolating: true,

  addOptions() {
    return {
      HTMLAttributes: {},
    };
  },

  parseHTML() {
    return [
      {
        tag: 'div[data-frontmatter]',
        preserveWhitespace: "full" as const,
      },
    ];
  },

  renderHTML({ HTMLAttributes }) {
    return [
      "div",
      mergeAttributes(this.options.HTMLAttributes, HTMLAttributes, {
        "data-frontmatter": "",
        class: "frontmatter-block",
        style: [
          "background: #1E1A16",
          "border-top: 2px solid #5C4033",
          "border-radius: 4px",
          "padding: 12px 16px",
          "margin-bottom: 16px",
          "font-family: 'JetBrains Mono', monospace",
          "font-size: 12px",
          "color: #5A524A",
          "white-space: pre",
          "line-height: 1.6",
        ].join("; "),
      }),
      0,
    ];
  },

  addCommands() {
    return {
      setFrontmatter:
        (content: string) =>
        ({ commands }) => {
          return commands.insertContent({
            type: this.name,
            content: [{ type: "text", text: content }],
          });
        },
    };
  },

  addProseMirrorPlugins() {
    return [
      new Plugin({
        key: new PluginKey("frontmatterDetect"),
        appendTransaction: (_transactions, _oldState, newState) => {
          // Detect frontmatter at document start: ---\n...\n---
          const doc = newState.doc;
          const firstChild = doc.firstChild;
          if (!firstChild) return null;

          // If the first node is already a frontmatter block, skip
          if (firstChild.type.name === "frontmatterBlock") return null;

          // Check if first text node starts with ---
          if (firstChild.type.name !== "paragraph") return null;
          const text = firstChild.textContent;
          if (!text.startsWith("---")) return null;

          // Look for closing --- in subsequent paragraphs
          let endPos = -1;
          let endNodeEnd = -1;
          doc.forEach((node: ProsemirrorNode, offset: number) => {
            if (offset === 0) return; // skip first node
            if (endPos !== -1) return; // already found
            if (node.type.name === "paragraph" && node.textContent.trim() === "---") {
              endPos = offset;
              endNodeEnd = offset + node.nodeSize;
            }
          });

          if (endPos === -1) return null;

          // Collect all text between the --- markers
          let frontmatterText = "";
          doc.forEach((node: ProsemirrorNode, offset: number) => {
            if (offset === 0) {
              frontmatterText = node.textContent.slice(3).trim(); // remove leading ---
            } else if (offset < endPos) {
              frontmatterText += "\n" + node.textContent;
            }
          });

          if (!frontmatterText.trim()) return null;

          const tr = newState.tr;
          const frontmatterNode = newState.schema.nodes.frontmatterBlock.create(
            {},
            frontmatterText.trim() ? [newState.schema.text(frontmatterText.trim())] : []
          );

          tr.replaceWith(0, endNodeEnd, frontmatterNode);
          return tr;
        },
      }),
    ];
  },
});
