interface AccountIdentity<Owner> {
  owner: Owner;
  credential: string;
}

/** Pins asynchronous work to the account and Telegram session present on mount. */
export function createOwnedSession<Owner>(
  initial: AccountIdentity<Owner>,
  currentIdentity: () => AccountIdentity<Owner>,
  authError: () => Error,
) {
  const { owner, credential } = initial;
  function isCurrent() {
    const current = currentIdentity();
    return current.owner === owner && current.credential === credential;
  }
  function assertCurrent() {
    if (!isCurrent()) throw authError();
  }
  async function request<T>(operation: () => Promise<T>): Promise<T> {
    // The operation builds its authorization header synchronously, so no account
    // change can occur between this check and dispatching the request.
    assertCurrent();
    try {
      const response = await operation();
      assertCurrent();
      return response;
    } catch (error) {
      // An old account's 404 must not trigger a recovery GET as the new account.
      assertCurrent();
      throw error;
    }
  }
  // Credentials stay private to this closure, never in query keys or messages.
  return { owner, isCurrent, assertCurrent, request } as const;
}
