use super::*;
use crate::management::{
    CommandReceipt, ResolveStopped, ReviewStopped, StoppedInspection, StoppedListQuery,
    StoppedPage, StoppedQuery, StoppedResolution, StoppedReview,
};

impl CurrentManagementStore {
    pub(crate) async fn stopped_tasks(
        &self,
        profile: &ProfileAuthority,
        query: StoppedListQuery,
    ) -> Result<StoppedPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_stopped_tasks(&query, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn stopped_inspection(
        &self,
        profile: &ProfileAuthority,
        query: StoppedQuery,
    ) -> Result<StoppedInspection, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_stopped_inspection(&query, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn review_stopped(
        &self,
        profile: &ProfileAuthority,
        input: ReviewStopped,
        receipt: CommandReceipt,
    ) -> Result<StoppedReview, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_review_stopped(&input, receipt, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub(crate) async fn resolve_stopped(
        &self,
        profile: &ProfileAuthority,
        input: ResolveStopped,
        receipt: CommandReceipt,
        inspected: CommandReceipt,
        request_id: String,
    ) -> Result<StoppedResolution, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(32 * 1024, move |db| {
                db.management_resolve_stopped(&input, receipt, inspected, &request_id, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
