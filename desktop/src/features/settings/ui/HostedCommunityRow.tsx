import * as React from "react";
import {
  Archive,
  ArchiveRestore,
  ArrowLeftRight,
  LoaderCircle,
} from "lucide-react";

import {
  hostedCommunityRelayUrl as relayUrl,
  type HostedCommunity,
} from "@/features/communities/hostedCommunityApi";
import { CommunityIconSettingsCard } from "@/features/communities/ui/CommunityIconSettingsCard";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/shared/ui/alert-dialog";
import { Button, buttonVariants } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { HostedCommunityDeleteDialog } from "./HostedCommunityDeleteDialog";

export function HostedCommunityRow({
  community,
  busy,
  canDelete,
  canConnect,
  deletionPending,
  onConnect,
  onArchive,
  onUnarchive,
  onTransfer,
  onDelete,
  showIconPicker,
}: {
  community: HostedCommunity;
  busy: boolean;
  canDelete: boolean;
  canConnect: boolean;
  deletionPending: boolean;
  onConnect: () => void;
  onArchive: () => void;
  onUnarchive: () => void;
  onTransfer: (npub: string) => Promise<boolean>;
  onDelete: () => void;
  showIconPicker: boolean;
}) {
  const [confirmArchive, setConfirmArchive] = React.useState(false);
  const [confirmUnarchive, setConfirmUnarchive] = React.useState(false);
  const [transferOpen, setTransferOpen] = React.useState(false);
  const url = relayUrl(community);
  const archived = Boolean(community.archived_at);
  const displayName = community.name ?? community.slug ?? "Hosted community";

  return (
    <li
      className={`flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border/70 p-4 ${
        archived ? "opacity-70" : ""
      }`}
      data-testid="hosted-community-row"
    >
      <div className="flex min-w-0 flex-1 items-center gap-3">
        {showIconPicker ? <CommunityIconSettingsCard compact /> : null}
        <div className="min-w-0">
          <p className="truncate text-sm font-medium">{displayName}</p>
          <p
            className="truncate text-xs text-muted-foreground/70"
            data-settings-subcopy
          >
            {community.normalized_host}
            {archived ? " · Archived" : ""}
          </p>
        </div>
      </div>

      {archived ? (
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={busy || !community.id}
            onClick={() => setConfirmUnarchive(true)}
          >
            <ArchiveRestore className="h-4 w-4" /> Unarchive
          </Button>
          <AlertDialog
            open={confirmUnarchive}
            onOpenChange={setConfirmUnarchive}
          >
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Unarchive {displayName}?</AlertDialogTitle>
                <AlertDialogDescription>
                  This address becomes connectable again. Connections that
                  closed during archival will not reconnect automatically.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction onClick={onUnarchive}>
                  Unarchive
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
          {canDelete ? (
            <HostedCommunityDeleteDialog
              community={community}
              disabled={busy || deletionPending}
              onConfirm={onDelete}
            />
          ) : null}
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          {url && canConnect ? (
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={onConnect}
            >
              Connect
            </Button>
          ) : null}
          <Button
            variant="ghost"
            size="sm"
            disabled={busy || !community.id}
            onClick={() => setTransferOpen(true)}
          >
            <ArrowLeftRight className="h-4 w-4" /> Transfer
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-destructive hover:text-destructive"
            disabled={busy || !community.id}
            onClick={() => setConfirmArchive(true)}
          >
            <Archive className="h-4 w-4" /> Archive
          </Button>

          <AlertDialog open={confirmArchive} onOpenChange={setConfirmArchive}>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Archive {displayName}?</AlertDialogTitle>
                <AlertDialogDescription>
                  New and existing connections stop and the address stays
                  reserved. Archiving can&apos;t be undone from here without
                  unarchiving, and the community keeps counting toward your
                  quota — it isn&apos;t deleted.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className={buttonVariants({ variant: "destructive" })}
                  onClick={onArchive}
                >
                  Archive
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>

          <TransferOwnershipDialog
            open={transferOpen}
            onOpenChange={setTransferOpen}
            communityName={displayName}
            busy={busy}
            onTransfer={onTransfer}
          />
        </div>
      )}
    </li>
  );
}

function TransferOwnershipDialog({
  open,
  onOpenChange,
  communityName,
  busy,
  onTransfer,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  communityName: string;
  busy: boolean;
  onTransfer: (npub: string) => Promise<boolean>;
}) {
  const [npub, setNpub] = React.useState("");
  const npubIsValid = npub.startsWith("npub1") && npub.length >= 50;

  React.useEffect(() => {
    if (!open) setNpub("");
  }, [open]);

  const submit = async () => {
    if (!npubIsValid) return;
    const ok = await onTransfer(npub.trim());
    if (ok) onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Transfer ownership</DialogTitle>
          <DialogDescription>
            Transfer {communityName} to another person. You become a regular
            member. The recipient needs a connected Buzz identity first, and
            this can&apos;t be undone.
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-2">
          <Input
            aria-label="Recipient npub"
            autoComplete="off"
            className="font-mono text-sm"
            placeholder="npub1…"
            spellCheck={false}
            value={npub}
            onChange={(event) => setNpub(event.target.value.trim())}
          />
          {npub.length > 0 && !npubIsValid ? (
            <p className="text-sm text-destructive">
              Enter a valid npub that starts with npub1.
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            disabled={!npubIsValid || busy}
            onClick={() => void submit()}
          >
            {busy ? <LoaderCircle className="h-4 w-4 animate-spin" /> : null}
            Transfer ownership
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
