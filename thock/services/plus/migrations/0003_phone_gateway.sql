-- Ask on the phone (spec: thock/specs/v35-phone-ask.md §5.1). The phone gets
-- its own budget-capped gateway key beside the desk's, so it can be revoked
-- with the phone; the allowance stays one pool summed over both. An empty
-- hash means the user has no phone key.

alter table users add column phone_gateway_key_hash text not null default '';
alter table users add column phone_gateway_key_secret text not null default '';
alter table users add column phone_gateway_limit_usd double precision not null default 0;
alter table users add column phone_usage_baseline_usd double precision not null default 0;
