// The Silicon Accounts testkit's helper library (../silicon-accounts/testkit/lib): sign-in flows over HTTP, the
// mock email outbox, PKCE, cookie jars. Calc's e2e code imports it from here, so the path lives in one place.
//
// Node runs the testkit's TypeScript as it is (type stripping), so nothing has to be installed for Calc.

export * from '../../../silicon-accounts/testkit/lib/index.ts';
