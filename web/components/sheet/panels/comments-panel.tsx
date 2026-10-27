"use client";

/**
 * Comments: threads on cells, ranges, rows, columns and whole tabs. A new comment starts from the selection (the
 * toolbar's button, the context menu, Ctrl/⌘ + Alt + M) and @mentions Carbons and Silicons by id, suggested as they are
 * typed. Each thread says where it is (jump to its cells), quotes what the cells showed when it was made, and holds
 * its replies, reactions, edits and resolving (Silicon UI's CommentThread). Threads arrive live from everyone.
 */
import { useMemo, useState } from "react";
import { MessageSquareText } from "lucide-react";
import { CommentThread, type CommentAuthor, type ThreadComment } from "@/components/silicon-ui/comment-thread/comment-thread";
import { MentionInput, type MentionPerson, type MentionValue } from "@/components/silicon-ui/mention-input/mention-input";
import SegmentedControl from "@/components/silicon-ui/segmented-control/segmented-control";
import { Button } from "@/components/silicon-ui/button/button";
import * as calc from "@/lib/calc/api";
import type { Account, Comment, Thread } from "@/lib/calc/types";
import { ApiError } from "@/lib/errors";
import { formatRelative, plural } from "@/lib/format";
import { notifyError } from "@/lib/notify";
import { useSessionRevision } from "@/lib/sheet/react";
import { useEditor } from "../editor-context";
import { personOf, useMentionPeople } from "./mentions";
import styles from "./panels.module.css";

export interface CommentTarget {
  tab: number;
  tabName: string;
  /** The range (`B2:B9`, `B:B`, `3:3`), or null for the whole tab. */
  range: string | null;
}

const authorOf = (account: Account): CommentAuthor => ({
  id: account.uuid,
  name: account.display_name && account.display_name !== account.id ? `${account.display_name} · ${account.id}` : account.id,
  avatar: account.pfp_url ?? undefined,
});

/** Where a thread is, in words: "Sales · B2:B9", "Sales · row 3", "Sales · column B", "Sales · the whole tab". */
export function placeOf(tabName: string, range: string | null): string {
  if (!range) return `${tabName} · the whole tab`;
  const rows = /^(\d+):(\d+)$/.exec(range);
  if (rows) return `${tabName} · ${rows[1] === rows[2] ? `row ${rows[1]}` : `rows ${rows[1]}–${rows[2]}`}`;
  const cols = /^([A-Z]+):([A-Z]+)$/i.exec(range);
  if (cols) return `${tabName} · ${cols[1] === cols[2] ? `column ${cols[1]}` : `columns ${cols[1]}–${cols[2]}`}`;
  return `${tabName} · ${range}`;
}

function toThreadComments(thread: Thread): ThreadComment[] {
  const [first, ...rest] = thread.comments;
  if (!first) return [];
  const convert = (comment: Comment): ThreadComment => ({
    id: comment.id,
    author: authorOf(comment.author),
    body: comment.body,
    createdAt: formatRelative(comment.created_at),
    edited: !!comment.edited_at,
    reactions: comment.reactions.map(reaction => ({ emoji: reaction.emoji, users: reaction.accounts.map(account => account.uuid) })),
  });
  return [{ ...convert(first), replies: rest.map(convert) }];
}

function ThreadItem({ thread, people, me, canComment }: { thread: Thread; people: MentionPerson[]; me: Account; canComment: boolean }) {
  const { controller, upsertThread } = useEditor();
  const comments = useMemo(() => toThreadComments(thread), [thread]);
  const mentionPeople = useMemo(() => people.map(person => ({ id: person.id, name: person.name, avatar: person.avatar })), [people]);

  const withComment = (comment: Comment) => upsertThread({ ...thread, comments: thread.comments.some(c => c.id === comment.id) ? thread.comments.map(c => (c.id === comment.id ? comment : c)) : [...thread.comments, comment] });

  return (
    <li className={styles.item} data-testid="comment-thread" data-thread={thread.id}>
      <div className={styles.row}>
        <button type="button" className={styles.place} data-sq="surface" disabled={thread.detached} onClick={() => controller.focusRange(thread.tab, thread.range)} aria-label={`Go to ${placeOf(thread.tab_name, thread.range)}`}>
          {placeOf(thread.tab_name, thread.range)}
        </button>
        {thread.detached ? <span className={styles.muted}>Its cells were deleted</span> : null}
      </div>
      {thread.quote ? <p className={styles.quote}>{thread.quote}</p> : null}
      <CommentThread
        comments={comments}
        currentUser={authorOf(me)}
        people={mentionPeople}
        resolved={thread.status === "resolved"}
        maxDepth={1}
        placeholder={canComment ? "Reply, or @mention a c: or si: id" : "You can read comments here"}
        title={undefined}
        labels={{ thread: `Comments on ${placeOf(thread.tab_name, thread.range)}`, empty: "No comments in this thread." }}
        onResolvedChange={async resolved => {
          try {
            upsertThread(await (resolved ? calc.resolveThread(thread.id) : calc.reopenThread(thread.id)));
          } catch (error) {
            notifyError(error, resolved ? "Could not resolve the thread" : "Could not reopen the thread");
          }
        }}
        onCommentsChange={async (_next, event) => {
          if (!canComment) return;
          try {
            if (event.type === "reply") withComment(await calc.reply(thread.id, event.comment.body));
            else if (event.type === "edit") withComment(await calc.editComment(event.id, event.body));
            else if (event.type === "delete") {
              await calc.deleteComment(event.id);
              upsertThread({ ...thread, comments: thread.comments.filter(c => c.id !== event.id) });
            } else if (event.type === "react") {
              if (event.added) await calc.react(event.id, event.emoji);
              else await calc.unreact(event.id, event.emoji);
              upsertThread({
                ...thread,
                comments: thread.comments.map(comment => {
                  if (comment.id !== event.id) return comment;
                  const existing = comment.reactions.find(r => r.emoji === event.emoji);
                  const others = comment.reactions.filter(r => r.emoji !== event.emoji);
                  const accounts = event.added ? [...(existing?.accounts ?? []).filter(a => a.uuid !== me.uuid), me] : (existing?.accounts ?? []).filter(a => a.uuid !== me.uuid);
                  return { ...comment, reactions: accounts.length ? [...others, { emoji: event.emoji, accounts }] : others };
                }),
              });
            }
          } catch (error) {
            notifyError(error, "Could not change the comment");
          }
        }}
      />
    </li>
  );
}

function Composer({ target, known, onDone }: { target: CommentTarget; known: Account[]; onDone: () => void }) {
  const { line, upsertThread } = useEditor();
  const [value, setValue] = useState<MentionValue>({ text: "", mentions: [] });
  const people = useMentionPeople(known, value.text);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const send = async () => {
    const body = value.text.trim();
    if (!body) return;
    setBusy(true);
    setProblem(null);
    try {
      const thread = await calc.createThread(line, { tab: target.tab, range: target.range, body });
      upsertThread(thread);
      setValue({ text: "", mentions: [] });
      onDone();
    } catch (error) {
      const failure = ApiError.from(error);
      setProblem([failure.message, failure.hint].filter(Boolean).join(" "));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.composer} data-sq="surface" onSubmit={event => { event.preventDefault(); void send(); }} aria-label={`New comment on ${placeOf(target.tabName, target.range)}`}>
      <span className={styles.muted}>New comment on <strong style={{ color: "var(--foreground)", fontWeight: 500 }}>{placeOf(target.tabName, target.range)}</strong></span>
      <MentionInput
        aria-label="Comment"
        placeholder="Write a comment. @si:analyst to ask a Silicon."
        value={value}
        onChange={setValue}
        people={people}
        minRows={2}
        maxRows={8}
        placement="bottom"
        onSubmit={() => void send()}
        submitOnEnter={false}
      />
      {problem ? <p role="alert" className={styles.secondary} style={{ color: "var(--danger)" }}>{problem}</p> : null}
      <div className={styles.actions}>
        <Button type="button" variant="ghost" size="sm" onClick={onDone}>Cancel</Button>
        <Button type="submit" size="sm" loading={busy} disabled={!value.text.trim()}>Comment</Button>
      </div>
    </form>
  );
}

export function CommentsPanel({ composing, onComposed }: { composing: CommentTarget | null; onComposed: () => void }) {
  const { threads, me, canComment, controller, session, sheet } = useEditor();
  useSessionRevision(session);
  const [status, setStatus] = useState<"open" | "resolved">("open");
  const known = useMemo(() => {
    const out = new Map<string, Account>();
    out.set(sheet.owner.uuid, sheet.owner);
    for (const account of session.accounts.values()) out.set(account.uuid, account);
    for (const thread of threads) for (const comment of thread.comments) {
      out.set(comment.author.uuid, comment.author);
      for (const mention of comment.mentions) out.set(mention.uuid, mention);
    }
    return [...out.values()];
    // The accounts map grows as cells and viewers arrive; the thread list changes with comments.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threads, sheet.owner, session.accounts.size]);
  const people = useMemo(() => known.map(personOf), [known]);
  const shown = threads
    .filter(thread => thread.status === status)
    .sort((a, b) => (a.tab === controller.activeTab ? 0 : 1) - (b.tab === controller.activeTab ? 0 : 1) || b.created_at.localeCompare(a.created_at));
  const openCount = threads.filter(thread => thread.status === "open").length;
  const resolvedCount = threads.length - openCount;

  return (
    <div className={styles.panel}>
      {composing && canComment ? <Composer target={composing} known={known} onDone={onComposed} /> : null}
      <div className={styles.toolbar}>
        <SegmentedControl
          label="Which threads"
          value={status}
          onValueChange={value => setStatus(value as "open" | "resolved")}
          options={[
            { value: "open", label: `Open${openCount ? ` ${openCount}` : ""}` },
            { value: "resolved", label: `Resolved${resolvedCount ? ` ${resolvedCount}` : ""}` },
          ]}
        />
      </div>
      {shown.length === 0 ? (
        <div className={styles.empty}>
          <MessageSquareText size={20} strokeWidth={1.5} aria-hidden="true" />
          <span className={styles.emptyTitle}>{status === "open" ? "No open comments" : "No resolved comments"}</span>
          <span>{status === "open" ? (canComment ? "Select cells, a row or a column and comment on them. Mention a Silicon with its si: id to ask it for something." : "Nobody has commented on this sheet yet.") : "Resolved threads show here, and can be reopened."}</span>
        </div>
      ) : (
        <ul className={styles.list} aria-label={plural(shown.length, "thread")}>
          {me ? shown.map(thread => <ThreadItem key={thread.id} thread={thread} people={people} me={me} canComment={canComment} />) : shown.map(thread => <ReadOnlyThread key={thread.id} thread={thread} />)}
        </ul>
      )}
    </div>
  );
}

/** A thread for someone signed out (a public sheet): read only. */
function ReadOnlyThread({ thread }: { thread: Thread }) {
  const { controller } = useEditor();
  return (
    <li className={styles.item}>
      <button type="button" className={styles.place} data-sq="surface" onClick={() => controller.focusRange(thread.tab, thread.range)}>{placeOf(thread.tab_name, thread.range)}</button>
      {thread.quote ? <p className={styles.quote}>{thread.quote}</p> : null}
      {thread.comments.map(comment => (
        <div key={comment.id} className={styles.group}>
          <span className={styles.title}>{comment.author.display_name || comment.author.id} <span className={styles.muted}>{comment.author.id} · {formatRelative(comment.created_at)}</span></span>
          <span className={styles.secondary} style={{ whiteSpace: "pre-wrap" }}>{comment.body}</span>
        </div>
      ))}
    </li>
  );
}
