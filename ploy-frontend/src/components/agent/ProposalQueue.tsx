import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Link } from 'react-router-dom';
import { api } from '@/services/api';
import type { SafetyProposal } from '@/types';
import { Badge } from '@/components/ui/Badge';
import { Button } from '@/components/ui/Button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { formatTimestamp } from '@/lib/utils';

interface ProposalQueueProps {
  proposals: SafetyProposal[];
}

function statusVariant(status: SafetyProposal['status']) {
  switch (status) {
    case 'approved':
      return 'success' as const;
    case 'rejected':
    case 'failed':
      return 'destructive' as const;
    case 'pending':
    default:
      return 'warning' as const;
  }
}

export function ProposalQueue({ proposals }: ProposalQueueProps) {
  const queryClient = useQueryClient();
  const [busyProposalId, setBusyProposalId] = useState<string | null>(null);

  const refresh = async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['proposals'] }),
      queryClient.invalidateQueries({ queryKey: ['deployments'] }),
      queryClient.invalidateQueries({ queryKey: ['system-diagnostics'] }),
    ]);
  };

  const approveMutation = useMutation({
    mutationFn: async (proposalId: string) => {
      setBusyProposalId(proposalId);
      return api.approveProposal(proposalId, 'approved from oversight console');
    },
    onSettled: async () => {
      setBusyProposalId(null);
      await refresh();
    },
  });

  const rejectMutation = useMutation({
    mutationFn: async (proposalId: string) => {
      setBusyProposalId(proposalId);
      return api.rejectProposal(proposalId, 'rejected from oversight console');
    },
    onSettled: async () => {
      setBusyProposalId(null);
      await refresh();
    },
  });

  return (
    <Card>
      <CardHeader>
        <CardTitle className="text-lg">Proposal Queue</CardTitle>
      </CardHeader>
      <CardContent>
        {proposals.length === 0 ? (
          <p className="text-sm text-muted-foreground">No safety proposals yet</p>
        ) : (
          <div className="space-y-4">
            {proposals.map((proposal) => {
              const isBusy = busyProposalId === proposal.proposal_id;
              const isPending = proposal.status === 'pending';
              return (
                <div key={proposal.proposal_id} className="rounded-lg border p-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="space-y-2">
                      <div className="flex flex-wrap items-center gap-2">
                        <Link
                          to={`/oversight/proposals/${encodeURIComponent(proposal.proposal_id)}`}
                          className="font-medium text-primary hover:underline"
                        >
                          {proposal.action_kind} {proposal.target_deployment_id}
                        </Link>
                        <Badge variant={statusVariant(proposal.status)}>{proposal.status}</Badge>
                      </div>
                      <div className="text-sm text-muted-foreground">{proposal.rationale}</div>
                      <div className="text-xs text-muted-foreground">
                        created {formatTimestamp(proposal.created_at)}
                      </div>
                      {proposal.proposed_max_gross_exposure && (
                        <div className="text-xs text-muted-foreground">
                          proposed max exposure {proposal.proposed_max_gross_exposure}
                        </div>
                      )}
                    </div>

                    {isPending && (
                      <div className="flex gap-2">
                        <Button
                          size="sm"
                          onClick={() => approveMutation.mutate(proposal.proposal_id)}
                          disabled={isBusy}
                        >
                          Approve
                        </Button>
                        <Button
                          size="sm"
                          variant="destructive"
                          onClick={() => rejectMutation.mutate(proposal.proposal_id)}
                          disabled={isBusy}
                        >
                          Reject
                        </Button>
                      </div>
                    )}
                  </div>

                  {proposal.evidence.length > 0 && (
                    <div className="mt-3 space-y-1">
                      {proposal.evidence.map((item, index) => (
                        <div
                          key={`${proposal.proposal_id}-evidence-${index}`}
                          className="rounded bg-muted px-3 py-2 text-xs text-muted-foreground"
                        >
                          {item}
                        </div>
                      ))}
                    </div>
                  )}

                  {proposal.decision_note && (
                    <div className="mt-3 text-xs text-muted-foreground">
                      note {proposal.decision_note}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
