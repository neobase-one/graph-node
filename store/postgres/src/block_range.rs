use diesel::pg::Pg;
use diesel::query_builder::{AstPass, QueryFragment};
use diesel::result::QueryResult;
///! Utilities to deal with block numbers and block ranges
use diesel::serialize::{Output, ToSql};
use diesel::sql_types::{Integer, Range};
use lazy_static::lazy_static;
use std::env;
use std::io::Write;
use std::ops::{Bound, RangeBounds, RangeFrom};

use graph::prelude::{BlockNumber, BLOCK_NUMBER_MAX};

use crate::history_event::HistoryEvent;

lazy_static! {
    // Make it possible to turn off the expanded BlockRangeContainsClause
    // if that causes problems in the hosted service. Remove this variable
    // once we know that it does not lead to performance issues
    // Setting this to anything will disable the expanded BlockRangeContainsClause
    static ref DISABLE_BRIN_BLOCK_RANGE: bool = {
        env::var("DISABLE_BRIN_BLOCK_RANGE")
            .ok().is_some()
    };
}

/// The name of the column in which we store the block range
pub(crate) const BLOCK_RANGE_COLUMN: &str = "block_range";

/// The SQL clause we use to check that an entity version is current;
/// that version has an unbounded block range, but checking for
/// `upper_inf(block_range)` is slow and can't use the exclusion
/// index we have on entity tables; we therefore check if i32::MAX is
/// in the range
pub(crate) const BLOCK_RANGE_CURRENT: &str = "block_range @> 2147483647";

/// Most subgraph metadata entities are not versioned. For such entities, we
/// want two things:
///   - any CRUD operation modifies such an entity in place
///   - queries by a block number consider such an entity as present for
///     any block number
/// We therefore mark such entities with a block range `[-1,\infinity)`; we
/// use `-1` as the lower bound to make it easier to identify such entities
/// for troubleshooting/debugging
pub(crate) const BLOCK_UNVERSIONED: i32 = -1;

/// The range of blocks for which an entity is valid. We need this struct
/// to bind ranges into Diesel queries.
#[derive(Clone, Debug)]
pub struct BlockRange(Bound<BlockNumber>, Bound<BlockNumber>);

// Doing this properly by implementing Clone for Bound is currently
// a nightly-only feature, so we need to work around that
fn clone_bound(bound: Bound<&BlockNumber>) -> Bound<BlockNumber> {
    match bound {
        Bound::Included(nr) => Bound::Included(*nr),
        Bound::Excluded(nr) => Bound::Excluded(*nr),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Return the block number contained in the history event. If it is
/// `None` panic because that indicates that we want to perform an
/// operation that does not record history, which should not happen
/// with how we currently use relational schemas
pub(crate) fn block_number(history_event: &HistoryEvent) -> BlockNumber {
    let block_ptr = history_event.block_ptr;
    if block_ptr.number < std::i32::MAX as u64 {
        block_ptr.number as i32
    } else {
        panic!(
            "Block numbers bigger than {} are not supported, but received block number {}",
            std::i32::MAX,
            block_ptr.number
        )
    }
}

impl From<RangeFrom<BlockNumber>> for BlockRange {
    fn from(range: RangeFrom<BlockNumber>) -> BlockRange {
        BlockRange(
            clone_bound(range.start_bound()),
            clone_bound(range.end_bound()),
        )
    }
}

impl ToSql<Range<Integer>, Pg> for BlockRange {
    fn to_sql<W: Write>(&self, out: &mut Output<W, Pg>) -> diesel::serialize::Result {
        let pair = (self.0, self.1);
        ToSql::<Range<Integer>, Pg>::to_sql(&pair, out)
    }
}

/// Generate the clause that checks whether `block` is in the block range
/// of an entity
#[derive(Constructor)]
pub struct BlockRangeContainsClause<'a> {
    table_prefix: &'a str,
    block: BlockNumber,
}

impl<'a> QueryFragment<Pg> for BlockRangeContainsClause<'a> {
    fn walk_ast(&self, mut out: AstPass<Pg>) -> QueryResult<()> {
        out.unsafe_to_cache_prepared();

        // Generate
        //
        //       block_range @> {block}
        //   and coalesce(upper(block_range), BLOCK_NUMBER_MAX) > {block}
        //   and lower(block_range) <= {block}
        //
        // The last two (redundant) clauses are there to make the BRIN index
        // on block_range usable for these queries

        out.push_sql(self.table_prefix);
        out.push_identifier(BLOCK_RANGE_COLUMN)?;
        out.push_sql(" @> /* contains */ ");
        out.push_bind_param::<Integer, _>(&self.block)?;
        if !*DISABLE_BRIN_BLOCK_RANGE && self.block < BLOCK_NUMBER_MAX {
            // When block is BLOCK_NUMBER_MAX, these checks would be wrong; we
            // don't worry about adding the equivalent in that case since
            // we generally only see BLOCK_NUMBER_MAX here for metadata
            // queries where block ranges don't matter anyway
            out.push_sql(" and coalesce(upper(");
            out.push_identifier(BLOCK_RANGE_COLUMN)?;
            out.push_sql("), 2147483647) > ");
            out.push_bind_param::<Integer, _>(&self.block)?;
            out.push_sql(" and lower(");
            out.push_identifier(BLOCK_RANGE_COLUMN)?;
            out.push_sql(") <= ");
            out.push_bind_param::<Integer, _>(&self.block)
        } else {
            Ok(())
        }
    }
}

#[test]
fn block_number_max_is_i32_max() {
    // The code in this file embeds i32::MAX aka BLOCK_NUMBER_MAX in strings
    // for efficiency. This assertion makes sure that BLOCK_NUMBER_MAX still
    // is what we think it is
    assert_eq!(2147483647, BLOCK_NUMBER_MAX);
}
