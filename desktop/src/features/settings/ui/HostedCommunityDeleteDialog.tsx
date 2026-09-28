import * as React from "react";

import type { HostedCommunity } from "@/features/communities/hostedCommunityApi";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";

export function deletionAcknowledgementMatches(value: string, host: string) {
  return value === host;
}

export function HostedCommunityDeleteDialog({
  community,
  disabled,
  onConfirm,
}: {
  community: HostedCommunity;
  disabled: boolean;
  onConfirm: () => void;
}) {
  const [open, setOpen] = React.useState(false);
  const [hostAcknowledgement, setHostAcknowledgement] = React.useState("");
  const [finalStep, setFinalStep] = React.useState(false);
  const host = community.normalized_host ?? "";

  const setDialogOpen = (next: boolean) => {
    setOpen(next);
    if (!next) {
      setHostAcknowledgement("");
      setFinalStep(false);
    }
  };

  return (
    <>
      <Button
        variant="destructive"
        size="sm"
        disabled={disabled || !community.id || !host}
        onClick={() => setOpen(true)}
      >
        Delete
      </Button>
      <Dialog open={open} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {finalStep
                ? "Permanently delete this community?"
                : `Delete ${host}?`}
            </DialogTitle>
            <DialogDescription>
              This cannot be cancelled by an owner. All community content will
              eventually be deleted, the host stays permanently reserved, and
              your quota slot is released only after logical cleanup completes.
            </DialogDescription>
          </DialogHeader>
          {finalStep ? (
            <p className="text-sm">
              You acknowledged <strong>{host}</strong>. This final confirmation
              immediately starts the irreversible deletion workflow.
            </p>
          ) : (
            <label
              className="space-y-2 text-sm"
              htmlFor={`delete-host-${community.id}`}
            >
              <span>Type the exact host to continue: {host}</span>
              <Input
                id={`delete-host-${community.id}`}
                autoComplete="off"
                value={hostAcknowledgement}
                onChange={(event) => setHostAcknowledgement(event.target.value)}
              />
            </label>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setDialogOpen(false)}>
              Cancel
            </Button>
            {finalStep ? (
              <Button
                variant="destructive"
                disabled={disabled}
                onClick={() => {
                  onConfirm();
                  setDialogOpen(false);
                }}
              >
                Delete community permanently
              </Button>
            ) : (
              <Button
                variant="destructive"
                disabled={
                  disabled ||
                  !deletionAcknowledgementMatches(hostAcknowledgement, host)
                }
                onClick={() => setFinalStep(true)}
              >
                Continue
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
