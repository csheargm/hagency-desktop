-- Each model remains bound to one owner-approved profile and its shared seat.
-- Preserve the earlier single-model resource and its original version proof.
CREATE TABLE owner_octos_model_resources (
 profile_digest TEXT NOT NULL REFERENCES owner_octos_profiles(profile_digest),
 model TEXT NOT NULL,
 resource_id TEXT NOT NULL UNIQUE REFERENCES resources(id),
 configuration_digest TEXT NOT NULL CHECK(length(configuration_digest)=64),
 PRIMARY KEY(profile_digest, model)
);
INSERT INTO owner_octos_model_resources(profile_digest,model,resource_id,configuration_digest)
 SELECT o.profile_digest,o.model,r.id,o.configuration_digest
 FROM owner_octos_profiles o
 JOIN owner_resource_accounts a ON a.provider_digest=o.profile_digest AND a.identity_kind='explicit_owner_mapping'
 JOIN resources r ON json_extract(r.config,'$.seatId')=a.seat_id
 WHERE json_extract(r.config,'$.framework')='octos'
   AND json_extract(r.config,'$.octosProfile')=o.profile_id
   AND json_extract(r.config,'$.model')=o.model;
