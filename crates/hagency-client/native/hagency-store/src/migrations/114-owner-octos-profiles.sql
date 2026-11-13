-- The key itself stays in the owner's private directory. Once a commitment
-- exists, losing or replacing the key must never create a new budget.
CREATE TABLE owner_octos_key (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 commitment TEXT NOT NULL CHECK(length(commitment)=64)
);
-- A local Octos configuration is deliberately not a verified provider account.
CREATE TABLE owner_octos_profiles (
 profile_digest TEXT PRIMARY KEY REFERENCES owner_resource_accounts(provider_digest),
 profile_id TEXT NOT NULL,
 model TEXT NOT NULL,
 configuration_digest TEXT NOT NULL CHECK(length(configuration_digest)=64)
);
-- Recover the exact approved request after a lost reply without re-reading
-- changed credentials or allowing a different command under the same ID.
CREATE TABLE owner_octos_enrollments (
 command_id TEXT PRIMARY KEY REFERENCES owner_resource_receipts(command_id),
 profile TEXT NOT NULL,
 input_digest TEXT NOT NULL CHECK(length(input_digest)=64),
 resource_id TEXT NOT NULL
);
