"use client";

/**
 * Who can be @mentioned in a comment: everyone the sheet already knows (its owner, the people sharing it, the comment
 * authors, who changed its cells, who is looking at it), plus whoever the id being typed is, looked up with
 * GET /v1/accounts/lookup once it reads as a whole `c:` or `si:` id. Suggestions insert `@c:ada`, which is how the
 * service reads mentions.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import type { MentionPerson } from "@/components/silicon-ui/mention-input/mention-input";
import { lookupAccount } from "@/lib/calc/api";
import type { Account } from "@/lib/calc/types";
import { kindNoun } from "@/lib/format";

const ID = /^(c|si):[a-z0-9][a-z0-9._-]{0,63}$/i;

/** The `@…` being typed at the end of the text, when it reads as an account id. */
export function typedMention(text: string): string | null {
  const match = /(?:^|[\s([{"'])@([^\s@]+)$/.exec(text);
  const id = match?.[1]?.replace(/[.,;:!?)]+$/, "") ?? "";
  return ID.test(id) ? id.toLowerCase() : null;
}

export function personOf(account: Account): MentionPerson {
  const name = account.display_name?.trim();
  return { id: account.uuid, name: account.id, role: name && name !== account.id ? `${name} · ${kindNoun(account.kind)}` : kindNoun(account.kind), avatar: account.pfp_url ?? undefined };
}

export function useMentionPeople(known: Account[], text: string): MentionPerson[] {
  const [found, setFound] = useState<Record<string, Account | null>>({});
  const typed = typedMention(text);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!typed || typed in found || known.some(account => account.id.toLowerCase() === typed)) return;
    if (timer.current) clearTimeout(timer.current);
    const controller = new AbortController();
    timer.current = setTimeout(() => {
      lookupAccount(typed, controller.signal)
        .then(account => setFound(current => ({ ...current, [typed]: account })))
        .catch(() => {
          if (!controller.signal.aborted) setFound(current => ({ ...current, [typed]: null }));
        });
    }, 220);
    return () => {
      controller.abort();
      if (timer.current) clearTimeout(timer.current);
    };
  }, [typed, found, known]);
  return useMemo(() => {
    const byUuid = new Map<string, Account>();
    for (const account of known) if (account?.uuid) byUuid.set(account.uuid, account);
    for (const account of Object.values(found)) if (account) byUuid.set(account.uuid, account);
    return [...byUuid.values()].sort((a, b) => a.id.localeCompare(b.id)).map(personOf);
  }, [known, found]);
}
