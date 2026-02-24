import { useEditor, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import Link from "@tiptap/extension-link";
import CodeBlockLowlight from "@tiptap/extension-code-block-lowlight";
import { Markdown } from "tiptap-markdown";
import { createLowlight, common } from "lowlight";
import { useEffect, useMemo, useCallback } from "react";
import { Save } from "lucide-react";
import type { VaultNote, VaultFrontmatter } from "../../types/vault";
import { WikiLink } from "./extensions/WikiLink";
import { TagMark } from "./extensions/TagMark";
import { useVaultStore } from "../../stores/vault";

const lowlight = createLowlight(common);

interface NoteEditorProps {
  note: VaultNote;
  onNavigate: (noteName: string) => void;
  onTagClick: (tag: string) => void;
  onChange?: (content: string) => void;
}

function FrontmatterBar({ frontmatter }: { frontmatter: VaultFrontmatter }) {
  const entries = Object.entries(frontmatter).filter(
    ([, v]) => v !== undefined
  );
  if (entries.length === 0) return null;

  return (
    <div
      className="border-t-2 border-[#5C4033] rounded mb-4"
      style={{ background: "#1E1A16", padding: "10px 14px" }}
    >
      <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
        {entries.map(([key, value]) => (
          <div key={key} className="contents">
            <span
              className="text-[#6B6258] text-xs font-mono"
              style={{ fontSize: "11px" }}
            >
              {key}
            </span>
            <span className="text-xs" style={{ fontSize: "11px" }}>
              {key === "tags" && Array.isArray(value) ? (
                <span className="flex flex-wrap gap-1">
                  {(value as string[]).map((tag) => (
                    <span
                      key={tag}
                      className="inline-block rounded-full px-2 py-0 text-[#DCD5CC]"
                      style={{
                        background: "#5C4033",
                        fontSize: "10px",
                        lineHeight: "1.6",
                      }}
                    >
                      {tag}
                    </span>
                  ))}
                </span>
              ) : (
                <span className="text-[#DCD5CC]">{String(value)}</span>
              )}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function stripFrontmatter(content: string): string {
  const match = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
  if (match) return match[2];
  return content;
}

export function NoteEditor({ note, onNavigate, onTagClick, onChange }: NoteEditorProps) {
  const saving = useVaultStore((s) => s.saving);
  const saveNote = useVaultStore((s) => s.saveNote);
  const markdownContent = useMemo(() => stripFrontmatter(note.content), [note.content]);

  const handleUpdate = useCallback(
    (markdown: string) => {
      // Reconstruct full content with frontmatter for save
      const fmMatch = note.content.match(/^(---\n[\s\S]*?\n---\n?)/);
      const fullContent = fmMatch ? fmMatch[1] + markdown : markdown;
      saveNote(note.path, fullContent);
      if (onChange) onChange(markdown);
    },
    [note.path, note.content, saveNote, onChange]
  );

  const editor = useEditor(
    {
      extensions: [
        StarterKit.configure({
          codeBlock: false,
        }),
        CodeBlockLowlight.configure({
          lowlight,
        }),
        Link.configure({
          openOnClick: false,
          HTMLAttributes: {
            style: "color: #5C4033; text-decoration: none;",
            class: "vault-link",
          },
        }),
        WikiLink.configure({
          onNavigate,
          onSuggestion: () => [],
        }),
        TagMark.configure({
          onTagClick,
        }),
        Markdown.configure({
          html: false,
          transformPastedText: true,
          transformCopiedText: true,
        }),
      ],
      content: markdownContent,
      editorProps: {
        attributes: {
          class: "vault-editor-content",
          spellcheck: "false",
        },
      },
      onUpdate: ({ editor: e }) => {
        const storage = (e.storage as unknown as Record<string, unknown>).markdown as { getMarkdown?: () => string } | undefined;
        if (storage?.getMarkdown) {
          handleUpdate(storage.getMarkdown());
        }
      },
    },
    [note.path]
  );

  useEffect(() => {
    if (editor && markdownContent !== undefined) {
      const storage = (editor.storage as unknown as Record<string, unknown>).markdown as { getMarkdown?: () => string } | undefined;
      const currentContent = storage?.getMarkdown?.() ?? "";
      if (currentContent !== markdownContent) {
        editor.commands.setContent(markdownContent);
      }
    }
  }, [editor, markdownContent]);

  return (
    <div className="flex flex-col h-full" style={{ background: "#0A0A0A" }}>
      {/* Header */}
      <div className="flex items-center px-4 py-2 border-b border-[#252018]">
        <span className="text-[#6B6258] text-xs font-mono">{note.path}</span>
        <span className="ml-auto flex items-center gap-2 text-[#4A4238] text-xs">
          {saving && (
            <span className="flex items-center text-[#8B7B3A]">
              <Save size={11} className="mr-1" />
              Saving...
            </span>
          )}
          {note.modified_at && new Date(note.modified_at).toLocaleString()}
        </span>
      </div>

      {/* Editor area */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        <FrontmatterBar frontmatter={note.frontmatter} />
        <EditorContent
          editor={editor}
          className="vault-editor"
        />
      </div>

      {/* Editor styles */}
      <style>{`
        .vault-editor .tiptap {
          outline: none;
          color: #DCD5CC;
          font-family: 'Inter', sans-serif;
          font-size: 14px;
          line-height: 1.7;
          min-height: 200px;
        }

        .vault-editor .tiptap h1 {
          color: #7A5A4A;
          font-size: 1.75em;
          font-weight: 600;
          letter-spacing: -0.02em;
          margin-top: 1.5em;
          margin-bottom: 0.5em;
          border-bottom: 1px solid #252018;
          padding-bottom: 0.3em;
        }

        .vault-editor .tiptap h2 {
          color: #7A5A4A;
          font-size: 1.4em;
          font-weight: 600;
          letter-spacing: -0.02em;
          margin-top: 1.3em;
          margin-bottom: 0.4em;
        }

        .vault-editor .tiptap h3 {
          color: #7A5A4A;
          font-size: 1.15em;
          font-weight: 600;
          letter-spacing: -0.02em;
          margin-top: 1.2em;
          margin-bottom: 0.3em;
        }

        .vault-editor .tiptap p {
          margin-bottom: 0.7em;
        }

        .vault-editor .tiptap a,
        .vault-editor .tiptap .vault-link {
          color: #5C4033;
          text-decoration: none;
        }

        .vault-editor .tiptap a:hover,
        .vault-editor .tiptap .vault-link:hover {
          text-decoration: underline;
        }

        .vault-editor .tiptap .wiki-link:hover {
          text-decoration-color: #5C4033 !important;
        }

        .vault-editor .tiptap code {
          background: #161210;
          color: #DCD5CC;
          padding: 2px 5px;
          border-radius: 3px;
          font-family: 'JetBrains Mono', monospace;
          font-size: 0.9em;
        }

        .vault-editor .tiptap pre {
          background: #161210;
          border-radius: 6px;
          padding: 14px 16px;
          margin: 1em 0;
          overflow-x: auto;
        }

        .vault-editor .tiptap pre code {
          background: none;
          padding: 0;
          font-family: 'JetBrains Mono', monospace;
          font-size: 13px;
          line-height: 1.5;
          color: #DCD5CC;
        }

        .vault-editor .tiptap ul,
        .vault-editor .tiptap ol {
          padding-left: 1.5em;
          margin-bottom: 0.7em;
        }

        .vault-editor .tiptap li {
          margin-bottom: 0.2em;
        }

        .vault-editor .tiptap li::marker {
          color: #5A524A;
        }

        .vault-editor .tiptap blockquote {
          border-left: 3px solid #5C4033;
          padding-left: 1em;
          margin-left: 0;
          color: #5A524A;
          font-style: italic;
        }

        .vault-editor .tiptap hr {
          border: none;
          border-top: 1px solid #252018;
          margin: 1.5em 0;
        }

        .vault-editor .tiptap strong {
          color: #DCD5CC;
          font-weight: 600;
        }

        .vault-editor .tiptap em {
          color: #DCD5CC;
          font-style: italic;
        }

        .vault-editor .tiptap table {
          border-collapse: collapse;
          width: 100%;
          margin: 1em 0;
        }

        .vault-editor .tiptap th,
        .vault-editor .tiptap td {
          border: 1px solid #252018;
          padding: 6px 12px;
          text-align: left;
        }

        .vault-editor .tiptap th {
          background: #1E1A16;
          font-weight: 600;
          color: #7A5A4A;
        }

        /* Syntax highlighting */
        .vault-editor .tiptap .hljs-comment,
        .vault-editor .tiptap .hljs-quote { color: #5A524A; }
        .vault-editor .tiptap .hljs-keyword,
        .vault-editor .tiptap .hljs-selector-tag { color: #8B6B5A; }
        .vault-editor .tiptap .hljs-string,
        .vault-editor .tiptap .hljs-addition { color: #4A7C59; }
        .vault-editor .tiptap .hljs-number,
        .vault-editor .tiptap .hljs-literal { color: #8B7B3A; }
        .vault-editor .tiptap .hljs-built_in,
        .vault-editor .tiptap .hljs-type { color: #4A6B8B; }
        .vault-editor .tiptap .hljs-attr,
        .vault-editor .tiptap .hljs-attribute { color: #7A5A4A; }
        .vault-editor .tiptap .hljs-deletion { color: #8B4A4A; }
        .vault-editor .tiptap .hljs-title,
        .vault-editor .tiptap .hljs-section { color: #7A5A4A; font-weight: 600; }
      `}</style>
    </div>
  );
}
