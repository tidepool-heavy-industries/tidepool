-- | Fixture for 'ExecutionProjectionTest.verifyHierarchicalTargetModule':
-- a hierarchical module compiled as the PRIMARY target, proving
-- 'Tidepool.GhcPipeline.targetModuleNameFor' recognises the declared dotted
-- module name (@Session.Val.G1@) instead of reducing the target to its bare
-- file basename (@G1@).
module Session.Val.G1 where

hierarchicalValue :: Int
hierarchicalValue = 41
