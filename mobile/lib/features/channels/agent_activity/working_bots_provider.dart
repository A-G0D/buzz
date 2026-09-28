import 'package:hooks_riverpod/hooks_riverpod.dart';

import '../../../shared/mentions/agent_identity_provider.dart';
import '../../../shared/profile/user_cache_provider.dart';
import '../channel_management_provider.dart';
import '../channel_typing_provider.dart';

/// Derived provider that computes which verified agent members in a channel are
/// currently typing (i.e. "working"). Returns a set of lowercase pubkeys.
///
/// Used by both the members button badge and the members sheet to avoid
/// duplicating the agent-typing cross-reference logic.
final workingBotPubkeysProvider = Provider.autoDispose
    .family<Set<String>, String>((ref, channelId) {
      final typingEntries = ref.watch(channelTypingProvider(channelId));
      final membersAsync = ref.watch(channelMembersProvider(channelId));
      final allMembers = membersAsync.asData?.value ?? const <ChannelMember>[];
      final userCache = ref.watch(userCacheProvider);
      final agentPubkeys = agentPubkeysWithChannelBots(
        knownAgentPubkeys: agentPubkeysWithProfileOwners(
          knownAgentPubkeys: ref.watch(knownAgentPubkeysProvider),
          profileOwnedAgentPubkeys: userCache.entries
              .where((entry) => entry.value.isAgent)
              .map((entry) => entry.key),
        ),
        channelBotPubkeys: allMembers
            .where((member) => member.isBot)
            .map((member) => member.pubkey),
      );

      return <String>{
        for (final entry in typingEntries)
          if (agentPubkeys.contains(entry.pubkey.toLowerCase()))
            entry.pubkey.toLowerCase(),
      };
    });
